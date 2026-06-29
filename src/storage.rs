use rusqlite::{params, Connection, OptionalExtension};
use std::env;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::{SystemTime, UNIX_EPOCH};

pub type Result<T> = std::result::Result<T, Error>;

const MAX_MESSAGE_IDS: usize = 96;
const HEAD_MESSAGE_IDS: usize = 16;
const MESSAGE_IDS_PER_LINE: usize = 8;

#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    Sql(rusqlite::Error),
    Invalid(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "{error}"),
            Self::Sql(error) => write!(f, "{error}"),
            Self::Invalid(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<rusqlite::Error> for Error {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sql(error)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Role {
    System,
    Developer,
    User,
    Assistant,
    Tool,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Developer => "developer",
            Self::User => "user",
            Self::Assistant => "assistant",
            Self::Tool => "tool",
        }
    }
}

impl FromStr for Role {
    type Err = Error;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "system" => Ok(Self::System),
            "developer" => Ok(Self::Developer),
            "user" => Ok(Self::User),
            "assistant" => Ok(Self::Assistant),
            "tool" => Ok(Self::Tool),
            other => Err(Error::Invalid(format!("invalid role `{other}`"))),
        }
    }
}

#[derive(Debug)]
pub struct Message {
    pub id: String,
    pub seq: i64,
    pub role: Role,
    pub text: String,
    pub source: Option<String>,
    pub is_human_prompt: bool,
    pub preserve_warning: bool,
}

#[derive(Debug)]
pub struct Compaction {
    pub id: String,
    pub from_msg_id: String,
    pub to_msg_id: String,
    pub summary: String,
    pub n_messages_replaced: i64,
    pub warning: Option<String>,
}

#[derive(Debug)]
pub struct CompactionInput {
    pub from_msg_id: String,
    pub to_msg_id: String,
    pub summary: String,
}

#[derive(Debug, Eq, PartialEq)]
pub struct CompactionStats {
    pub active_compactions: i64,
    pub total_known_messages_replaced: i64,
}

struct ValidatedCompactionInput {
    from_endpoint: String,
    to_endpoint: String,
    from_seq: i64,
    to_seq: i64,
    summary: String,
}

#[derive(Debug)]
pub enum VisibleEntry {
    Message(Message),
    Compaction(Compaction),
}

pub struct Store {
    conn: Connection,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let store = Self { conn };
        store.migrate()?;
        store.migrate_existing_messages()?;
        store.migrate_legacy_ids()?;
        Ok(store)
    }

    pub fn default_path() -> PathBuf {
        if let Some(path) = env::var_os("PCODX_DB") {
            return PathBuf::from(path);
        }
        if let Some(path) = env::var_os("XDG_DATA_HOME") {
            return PathBuf::from(path).join("pcodx/pcodx.sqlite3");
        }
        let home = env::var_os("HOME").unwrap_or_else(|| ".".into());
        PathBuf::from(home).join(".local/share/pcodx/pcodx.sqlite3")
    }

    pub fn create_session(&mut self, requested_id: Option<&str>, cwd: &Path) -> Result<String> {
        let session_id = requested_id
            .map(ToOwned::to_owned)
            .unwrap_or_else(new_session_id);
        let now_ms = now_unix_ms();
        self.conn.execute(
            "INSERT INTO sessions(id, cwd, created_at_ms, updated_at_ms)
             VALUES (?1, ?2, ?3, ?3)
             ON CONFLICT(id) DO UPDATE SET cwd=excluded.cwd, updated_at_ms=excluded.updated_at_ms",
            params![session_id, cwd.display().to_string(), now_ms],
        )?;
        Ok(session_id)
    }

    pub fn last_session_id(&self) -> Result<Option<String>> {
        self.conn
            .query_row(
                "SELECT id FROM sessions ORDER BY updated_at_ms DESC, created_at_ms DESC, id DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(Error::from)
    }

    pub fn session_exists(&self, session_id: &str) -> Result<bool> {
        self.conn
            .query_row(
                "SELECT 1 FROM sessions WHERE id = ?1",
                params![session_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .map(|value| value.is_some())
            .map_err(Error::from)
    }

    pub fn record_message(
        &mut self,
        session_id: &str,
        role: Role,
        text: &str,
        source: Option<&str>,
    ) -> Result<Message> {
        self.ensure_session(session_id)?;
        let tx = self.conn.transaction()?;
        let seq: i64 = tx.query_row(
            "SELECT COALESCE(MAX(seq), 0) + 1 FROM messages WHERE session_id = ?1",
            params![session_id],
            |row| row.get(0),
        )?;
        let id = format_msg_id(seq);
        let now_ms = now_unix_ms();
        let is_human_prompt = role == Role::User;
        let preserve_warning = matches!(role, Role::System | Role::Developer | Role::User);
        tx.execute(
            "INSERT INTO messages(id, session_id, seq, role, text, source, created_at_ms, is_human_prompt, preserve_warning)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![id, session_id, seq, role.as_str(), text, source, now_ms, is_human_prompt, preserve_warning],
        )?;
        touch_session_tx(&tx, session_id, now_ms)?;
        tx.commit()?;
        Ok(Message {
            id,
            seq,
            role,
            text: text.to_owned(),
            source: source.map(ToOwned::to_owned),
            is_human_prompt,
            preserve_warning,
        })
    }

    pub fn messages(&self, session_id: &str) -> Result<Vec<Message>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, seq, role, text, source, is_human_prompt, preserve_warning
             FROM messages
             WHERE session_id = ?1
             ORDER BY seq",
        )?;
        let rows = stmt.query_map(params![session_id], message_from_row)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Error::from)
    }

    pub fn compact(
        &mut self,
        session_id: &str,
        from_msg_id: &str,
        to_msg_id: &str,
        summary: &str,
    ) -> Result<Compaction> {
        let mut compactions = self.compact_ranges(
            session_id,
            vec![CompactionInput {
                from_msg_id: from_msg_id.to_owned(),
                to_msg_id: to_msg_id.to_owned(),
                summary: summary.to_owned(),
            }],
        )?;
        compactions.pop().ok_or_else(|| {
            Error::Invalid("internal error: single compaction returned no result".to_owned())
        })
    }

    pub fn compact_ranges(
        &mut self,
        session_id: &str,
        ranges: Vec<CompactionInput>,
    ) -> Result<Vec<Compaction>> {
        if ranges.is_empty() {
            return Err(Error::Invalid(
                "provide at least one compaction range".to_owned(),
            ));
        }
        self.ensure_session(session_id)?;
        let messages = self.messages(session_id)?;
        let visible_ids = self.visible_ids(session_id)?;
        let mut validated = Vec::with_capacity(ranges.len());
        for range in ranges {
            if range.summary.trim().is_empty() {
                return Err(Error::Invalid("summary must be non-empty".to_owned()));
            }
            ensure_visible_endpoint(&visible_ids, &range.from_msg_id)?;
            ensure_visible_endpoint(&visible_ids, &range.to_msg_id)?;
            let from_seq = self.boundary_seq(session_id, &range.from_msg_id, BoundarySide::From)?;
            let to_seq = self.boundary_seq(session_id, &range.to_msg_id, BoundarySide::To)?;
            if from_seq > to_seq {
                return Err(Error::Invalid(format!(
                    "{} comes after {}",
                    range.from_msg_id, range.to_msg_id
                )));
            }
            validated.push(ValidatedCompactionInput {
                from_endpoint: range.from_msg_id,
                to_endpoint: range.to_msg_id,
                from_seq,
                to_seq,
                summary: range.summary,
            });
        }
        validated.sort_by_key(|range| range.from_seq);
        for range in &validated {
            validate_tool_pair_boundary(&messages, range.from_seq, range.to_seq)?;
        }
        for pair in validated.windows(2) {
            let prior = &pair[0];
            let next = &pair[1];
            if prior.to_seq >= next.from_seq {
                return Err(Error::Invalid(format!(
                    "range starting at msg{} overlaps another requested range",
                    next.from_seq
                )));
            }
        }
        let mut covered_compaction_ids = Vec::new();
        for range in &validated {
            covered_compaction_ids.extend(self.covered_compaction_ids(
                session_id,
                range.from_seq,
                range.to_seq,
            )?);
        }
        covered_compaction_ids.sort();
        covered_compaction_ids.dedup();

        let mut outputs = Vec::with_capacity(validated.len());
        let mut storage_endpoints = Vec::with_capacity(validated.len());
        for range in &validated {
            let n_preserved: i64 = self.conn.query_row(
                "SELECT COUNT(*) FROM messages
                 WHERE session_id = ?1 AND seq BETWEEN ?2 AND ?3 AND preserve_warning = 1",
                params![session_id, range.from_seq, range.to_seq],
                |row| row.get(0),
            )?;
            let warning = (n_preserved > 0).then(|| {
                "range contains system, developer, or user messages; the summary must preserve active instructions and human intent".to_owned()
            });
            let stored_from_msg_id = self.message_id_by_seq(session_id, range.from_seq)?;
            let stored_to_msg_id = self.message_id_by_seq(session_id, range.to_seq)?;
            outputs.push(Compaction {
                id: String::new(),
                from_msg_id: range.from_endpoint.clone(),
                to_msg_id: range.to_endpoint.clone(),
                summary: range.summary.clone(),
                n_messages_replaced: range.to_seq - range.from_seq + 1,
                warning,
            });
            storage_endpoints.push((stored_from_msg_id, stored_to_msg_id));
        }

        let tx = self.conn.transaction()?;
        let first_seq: i64 = tx.query_row(
            "SELECT COALESCE(MAX(seq), 0) + 1 FROM compactions WHERE session_id = ?1",
            params![session_id],
            |row| row.get(0),
        )?;
        for id in covered_compaction_ids {
            tx.execute(
                "DELETE FROM compactions WHERE session_id = ?1 AND id = ?2",
                params![session_id, id],
            )?;
        }
        let now_ms = now_unix_ms();
        for (idx, range) in validated.iter().enumerate() {
            let seq = first_seq + i64::try_from(idx).unwrap_or(i64::MAX);
            let id = format_cmp_id(seq);
            let output = &mut outputs[idx];
            let (stored_from_msg_id, stored_to_msg_id) = &storage_endpoints[idx];
            output.id = id.clone();
            tx.execute(
                "INSERT INTO compactions(id, session_id, seq, from_msg_id, to_msg_id, from_seq, to_seq, summary, created_at_ms, n_messages_replaced)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    id,
                    session_id,
                    seq,
                    stored_from_msg_id,
                    stored_to_msg_id,
                    range.from_seq,
                    range.to_seq,
                    output.summary,
                    now_ms,
                    output.n_messages_replaced
                ],
            )?;
        }
        touch_session_tx(&tx, session_id, now_ms)?;
        tx.commit()?;
        Ok(outputs)
    }

    pub fn visible_entries(&self, session_id: &str) -> Result<Vec<VisibleEntry>> {
        let messages = self.messages(session_id)?;
        let compactions = self.compactions(session_id)?;
        let mut entries = Vec::new();
        let mut i = 0;
        while i < messages.len() {
            let message = &messages[i];
            if let Some(compaction) = compactions
                .iter()
                .find(|compaction| compaction.from_msg_id == message.id)
            {
                let to_seq = parse_msg_seq(&compaction.to_msg_id)?;
                entries.push(VisibleEntry::Compaction(Compaction {
                    id: compaction.id.clone(),
                    from_msg_id: compaction.from_msg_id.clone(),
                    to_msg_id: compaction.to_msg_id.clone(),
                    summary: compaction.summary.clone(),
                    n_messages_replaced: compaction.n_messages_replaced,
                    warning: compaction.warning.clone(),
                }));
                i = messages
                    .iter()
                    .position(|candidate| candidate.seq > to_seq)
                    .unwrap_or(messages.len());
            } else {
                entries.push(VisibleEntry::Message(Message {
                    id: message.id.clone(),
                    seq: message.seq,
                    role: message.role,
                    text: message.text.clone(),
                    source: message.source.clone(),
                    is_human_prompt: message.is_human_prompt,
                    preserve_warning: message.preserve_warning,
                }));
                i += 1;
            }
        }
        Ok(entries)
    }

    pub fn visible_ids(&self, session_id: &str) -> Result<Vec<String>> {
        self.visible_entries(session_id).map(|entries| {
            entries
                .into_iter()
                .map(|entry| match entry {
                    VisibleEntry::Message(message) => message.id,
                    VisibleEntry::Compaction(compaction) => compaction.id,
                })
                .collect()
        })
    }

    pub fn current_session_message_id_lines(&self, session_id: &str) -> Result<String> {
        let ids = self.visible_ids(session_id)?;
        if ids.is_empty() {
            return Ok("- No current-session message IDs are available yet.".to_owned());
        }
        let n_ids = ids.len();
        let visible: Vec<&str> = if n_ids <= MAX_MESSAGE_IDS {
            ids.iter().map(String::as_str).collect()
        } else {
            let mut truncated = Vec::with_capacity(MAX_MESSAGE_IDS);
            truncated.extend(ids[..HEAD_MESSAGE_IDS].iter().map(String::as_str));
            truncated.extend(
                ids[n_ids - (MAX_MESSAGE_IDS - HEAD_MESSAGE_IDS)..]
                    .iter()
                    .map(String::as_str),
            );
            truncated
        };
        let mut lines = Vec::new();
        for (idx, chunk) in visible.chunks(MESSAGE_IDS_PER_LINE).enumerate() {
            if n_ids > MAX_MESSAGE_IDS && idx * MESSAGE_IDS_PER_LINE == HEAD_MESSAGE_IDS {
                lines.push(format!(
                    "- ... {} older middle IDs omitted; use message search/read tools for current session history if you need them ...",
                    n_ids - MAX_MESSAGE_IDS
                ));
            }
            lines.push(format!("- {}", chunk.join(", ")));
        }
        Ok(lines.join("\n"))
    }

    pub fn compaction_stats(&self, session_id: &str) -> Result<CompactionStats> {
        self.ensure_session(session_id)?;
        self.conn
            .query_row(
                "SELECT COUNT(*), COALESCE(SUM(n_messages_replaced), 0)
                 FROM compactions
                 WHERE session_id = ?1",
                params![session_id],
                |row| {
                    Ok(CompactionStats {
                        active_compactions: row.get(0)?,
                        total_known_messages_replaced: row.get(1)?,
                    })
                },
            )
            .map_err(Error::from)
    }

    pub fn render_visible_context(&self, session_id: &str) -> Result<String> {
        let mut blocks = Vec::new();
        for entry in self.visible_entries(session_id)? {
            match entry {
                VisibleEntry::Message(message) => blocks.push(format!(
                    "{}\n<aboveturn id=\"{}\"/>",
                    message.text, message.id
                )),
                VisibleEntry::Compaction(compaction) => blocks.push(format!(
                    "{}\n<aboveturn id=\"{}\"/>",
                    compaction.summary, compaction.id
                )),
            }
        }
        Ok(blocks.join("\n\n"))
    }

    fn migrate(&self) -> Result<()> {
        self.conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS schema_meta(
              key TEXT PRIMARY KEY,
              value TEXT NOT NULL
            );
            INSERT INTO schema_meta(key, value) VALUES ('schema_version', '1')
              ON CONFLICT(key) DO NOTHING;
            CREATE TABLE IF NOT EXISTS sessions(
              id TEXT PRIMARY KEY,
              cwd TEXT NOT NULL,
              created_at_ms INTEGER NOT NULL,
              updated_at_ms INTEGER NOT NULL,
              upstream_session_id TEXT,
              kv_cache_boundary TEXT NOT NULL DEFAULT 'future_turn_only'
            );
            CREATE TABLE IF NOT EXISTS messages(
              id TEXT NOT NULL,
              session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
              seq INTEGER NOT NULL,
              role TEXT NOT NULL CHECK(role IN ('system', 'developer', 'user', 'assistant', 'tool')),
              text TEXT NOT NULL,
              source TEXT,
              created_at_ms INTEGER NOT NULL,
              is_human_prompt INTEGER NOT NULL CHECK(is_human_prompt IN (0, 1)),
              preserve_warning INTEGER NOT NULL CHECK(preserve_warning IN (0, 1)),
              PRIMARY KEY(session_id, id),
              UNIQUE(session_id, seq)
            );
            CREATE TABLE IF NOT EXISTS compactions(
              id TEXT NOT NULL,
              session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
              seq INTEGER NOT NULL,
              from_msg_id TEXT NOT NULL,
              to_msg_id TEXT NOT NULL,
              from_seq INTEGER NOT NULL,
              to_seq INTEGER NOT NULL,
              summary TEXT NOT NULL,
              created_at_ms INTEGER NOT NULL,
              n_messages_replaced INTEGER NOT NULL,
              PRIMARY KEY(session_id, id),
              UNIQUE(session_id, seq),
              CHECK(from_seq <= to_seq)
            );
            CREATE INDEX IF NOT EXISTS idx_messages_session_seq ON messages(session_id, seq);
            CREATE INDEX IF NOT EXISTS idx_compactions_session_range ON compactions(session_id, from_seq, to_seq);
            ",
        )?;
        Ok(())
    }

    fn migrate_existing_messages(&self) -> Result<()> {
        let columns = self.table_columns("messages")?;
        if columns.iter().any(|column| column == "preserve_warning") {
            if columns.iter().any(|column| column == "must_preserve") {
                self.rebuild_messages_without_must_preserve()?;
            }
            return Ok(());
        }
        self.conn.execute(
            "ALTER TABLE messages ADD COLUMN preserve_warning INTEGER NOT NULL DEFAULT 0 CHECK(preserve_warning IN (0, 1))",
            [],
        )?;
        if columns.iter().any(|column| column == "must_preserve") {
            self.conn
                .execute("UPDATE messages SET preserve_warning = must_preserve", [])?;
            self.rebuild_messages_without_must_preserve()?;
        } else {
            self.conn.execute(
                "UPDATE messages SET preserve_warning = CASE WHEN role IN ('system', 'developer', 'user') THEN 1 ELSE 0 END",
                [],
            )?;
        }
        Ok(())
    }

    fn rebuild_messages_without_must_preserve(&self) -> Result<()> {
        self.conn.execute_batch(
            "
            CREATE TABLE messages_new(
              id TEXT NOT NULL,
              session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
              seq INTEGER NOT NULL,
              role TEXT NOT NULL CHECK(role IN ('system', 'developer', 'user', 'assistant', 'tool')),
              text TEXT NOT NULL,
              source TEXT,
              created_at_ms INTEGER NOT NULL,
              is_human_prompt INTEGER NOT NULL CHECK(is_human_prompt IN (0, 1)),
              preserve_warning INTEGER NOT NULL CHECK(preserve_warning IN (0, 1)),
              PRIMARY KEY(session_id, id),
              UNIQUE(session_id, seq)
            );
            INSERT INTO messages_new(id, session_id, seq, role, text, source, created_at_ms, is_human_prompt, preserve_warning)
            SELECT id, session_id, seq, role, text, source, created_at_ms, is_human_prompt, preserve_warning
            FROM messages;
            DROP TABLE messages;
            ALTER TABLE messages_new RENAME TO messages;
            ",
        )?;
        Ok(())
    }

    fn migrate_legacy_ids(&self) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "UPDATE messages
             SET id = 'msg' || seq
             WHERE id != 'msg' || seq",
            [],
        )?;
        tx.execute(
            "UPDATE compactions
             SET id = 'cmp' || seq,
                 from_msg_id = 'msg' || from_seq,
                 to_msg_id = 'msg' || to_seq
             WHERE id != 'cmp' || seq
                OR from_msg_id != 'msg' || from_seq
                OR to_msg_id != 'msg' || to_seq",
            [],
        )?;
        tx.commit()?;
        Ok(())
    }

    fn table_columns(&self, table: &str) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(&format!("PRAGMA table_info({table})"))?;
        let rows = stmt.query_map([], |row| row.get(1))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Error::from)
    }

    fn ensure_session(&self, session_id: &str) -> Result<()> {
        let exists: Option<i64> = self
            .conn
            .query_row(
                "SELECT 1 FROM sessions WHERE id = ?1",
                params![session_id],
                |row| row.get(0),
            )
            .optional()?;
        if exists.is_some() {
            Ok(())
        } else {
            Err(Error::Invalid(format!("unknown session `{session_id}`")))
        }
    }

    fn message_seq(&self, session_id: &str, msg_id: &str) -> Result<i64> {
        parse_msg_seq(msg_id)?;
        self.conn
            .query_row(
                "SELECT seq FROM messages WHERE session_id = ?1 AND id = ?2",
                params![session_id, msg_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| Error::Invalid(format!("message `{msg_id}` not found")))
    }

    fn message_id_by_seq(&self, session_id: &str, seq: i64) -> Result<String> {
        self.conn
            .query_row(
                "SELECT id FROM messages WHERE session_id = ?1 AND seq = ?2",
                params![session_id, seq],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| Error::Invalid(format!("message seq `{seq}` not found")))
    }

    fn boundary_seq(&self, session_id: &str, id: &str, side: BoundarySide) -> Result<i64> {
        if id.starts_with("msg") {
            return self.message_seq(session_id, id);
        }
        parse_cmp_seq(id)?;
        self.conn
            .query_row(
                "SELECT from_seq, to_seq FROM compactions WHERE session_id = ?1 AND id = ?2",
                params![session_id, id],
                |row| match side {
                    BoundarySide::From => row.get(0),
                    BoundarySide::To => row.get(1),
                },
            )
            .optional()?
            .ok_or_else(|| Error::Invalid(format!("compaction `{id}` not found")))
    }

    fn covered_compaction_ids(
        &self,
        session_id: &str,
        from_seq: i64,
        to_seq: i64,
    ) -> Result<Vec<String>> {
        let n_partial: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM compactions
             WHERE session_id = ?1
             AND NOT (to_seq < ?2 OR from_seq > ?3)
             AND NOT (from_seq >= ?2 AND to_seq <= ?3)",
            params![session_id, from_seq, to_seq],
            |row| row.get(0),
        )?;
        if n_partial > 0 {
            return Err(Error::Invalid(
                "range partially overlaps an existing compaction; use visible cmp endpoints or choose a non-overlapping range".to_owned(),
            ));
        }
        let mut stmt = self.conn.prepare(
            "SELECT id FROM compactions
             WHERE session_id = ?1 AND from_seq >= ?2 AND to_seq <= ?3
             ORDER BY from_seq",
        )?;
        let rows = stmt.query_map(params![session_id, from_seq, to_seq], |row| row.get(0))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Error::from)
    }

    fn compactions(&self, session_id: &str) -> Result<Vec<Compaction>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, from_msg_id, to_msg_id, summary, n_messages_replaced
             FROM compactions
             WHERE session_id = ?1
             ORDER BY from_seq",
        )?;
        let rows = stmt.query_map(params![session_id], |row| {
            Ok(Compaction {
                id: row.get(0)?,
                from_msg_id: row.get(1)?,
                to_msg_id: row.get(2)?,
                summary: row.get(3)?,
                n_messages_replaced: row.get(4)?,
                warning: None,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Error::from)
    }
}

#[derive(Clone, Copy)]
enum BoundarySide {
    From,
    To,
}

fn message_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Message> {
    let role_text: String = row.get(2)?;
    let role = Role::from_str(&role_text).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(2, rusqlite::types::Type::Text, Box::new(error))
    })?;
    let is_human_prompt: i64 = row.get(5)?;
    let preserve_warning: i64 = row.get(6)?;
    Ok(Message {
        id: row.get(0)?,
        seq: row.get(1)?,
        role,
        text: row.get(3)?,
        source: row.get(4)?,
        is_human_prompt: is_human_prompt != 0,
        preserve_warning: preserve_warning != 0,
    })
}

fn ensure_visible_endpoint(visible_ids: &[String], id: &str) -> Result<()> {
    if visible_ids.iter().any(|visible_id| visible_id == id) {
        return Ok(());
    }
    Err(Error::Invalid(format!(
        "endpoint `{id}` is not visible in the current session; use visible msg/cmp endpoints"
    )))
}

fn validate_tool_pair_boundary(messages: &[Message], from_seq: i64, to_seq: i64) -> Result<()> {
    for group in tool_result_groups(messages) {
        let overlaps = from_seq <= group.last_tool_seq && to_seq >= group.assistant_seq;
        let covers = from_seq <= group.assistant_seq && to_seq >= group.last_tool_seq;
        if overlaps && !covers {
            if from_seq > group.assistant_seq {
                return Err(Error::Invalid(format!(
                    "range splits a tool_use/tool_result group at {}; extend the range start back to {} or start after {}",
                    group.first_tool_id, group.assistant_id, group.last_tool_id
                )));
            }
            return Err(Error::Invalid(format!(
                "range splits a tool_use/tool_result group at {}; extend the range to {}",
                group.assistant_id, group.last_tool_id
            )));
        }
    }
    Ok(())
}

struct ToolResultGroup<'a> {
    assistant_seq: i64,
    assistant_id: &'a str,
    first_tool_id: &'a str,
    last_tool_seq: i64,
    last_tool_id: &'a str,
}

fn tool_result_groups(messages: &[Message]) -> Vec<ToolResultGroup<'_>> {
    let mut groups = Vec::new();
    let mut i = 0;
    while i + 1 < messages.len() {
        let assistant = &messages[i];
        if assistant.role != Role::Assistant || messages[i + 1].role != Role::Tool {
            i += 1;
            continue;
        }
        let first_tool = &messages[i + 1];
        let mut last_tool = first_tool;
        i += 2;
        while i < messages.len() && messages[i].role == Role::Tool {
            last_tool = &messages[i];
            i += 1;
        }
        groups.push(ToolResultGroup {
            assistant_seq: assistant.seq,
            assistant_id: &assistant.id,
            first_tool_id: &first_tool.id,
            last_tool_seq: last_tool.seq,
            last_tool_id: &last_tool.id,
        });
    }
    groups
}

fn touch_session_tx(tx: &rusqlite::Transaction<'_>, session_id: &str, now_ms: i64) -> Result<()> {
    tx.execute(
        "UPDATE sessions SET updated_at_ms = ?2 WHERE id = ?1",
        params![session_id, now_ms],
    )?;
    Ok(())
}

fn now_unix_ms() -> i64 {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
}

fn new_session_id() -> String {
    format!("ses{:x}-{}", now_unix_ms(), std::process::id())
}

fn format_msg_id(seq: i64) -> String {
    format!("msg{seq}")
}

fn format_cmp_id(seq: i64) -> String {
    format!("cmp{seq}")
}

fn parse_msg_seq(msg_id: &str) -> Result<i64> {
    parse_prefixed_seq(msg_id, "msg")
}

fn parse_cmp_seq(cmp_id: &str) -> Result<i64> {
    parse_prefixed_seq(cmp_id, "cmp")
}

fn parse_prefixed_seq(id: &str, prefix: &str) -> Result<i64> {
    let suffix = id
        .strip_prefix(prefix)
        .ok_or_else(|| Error::Invalid(format!("invalid {prefix} id `{id}`")))?;
    if suffix.is_empty() || !suffix.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(Error::Invalid(format!("invalid {prefix} id `{id}`")));
    }
    let seq = suffix
        .parse::<i64>()
        .map_err(|_| Error::Invalid(format!("invalid {prefix} id `{id}`")))?;
    if seq < 1 {
        return Err(Error::Invalid(format!("invalid {prefix} id `{id}`")));
    }
    Ok(seq)
}

#[cfg(test)]
mod tests {
    use super::{CompactionInput, Role, Store};
    use rusqlite::{params, Connection};
    use tempfile::tempdir;

    #[test]
    fn sqlite_round_trip_renders_compacted_future_context() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("pcodx.sqlite3");
        let mut store = Store::open(&path).unwrap();
        let session = store.create_session(Some("ses-test"), temp.path()).unwrap();
        let stale = store
            .record_message(&session, Role::Assistant, "large stale output", None)
            .unwrap();
        store
            .record_message(&session, Role::Tool, "tool details", None)
            .unwrap();
        let kept = store
            .record_message(&session, Role::Assistant, "current result", None)
            .unwrap();
        let compaction = store
            .compact(&session, &stale.id, "msg2", "summary of stale work")
            .unwrap();
        assert_eq!(compaction.id, "cmp1");
        assert_eq!(
            store.visible_ids(&session).unwrap(),
            vec!["cmp1".to_owned(), kept.id]
        );
        let rendered = store.render_visible_context(&session).unwrap();
        assert!(rendered.contains("summary of stale work"));
        assert!(rendered.contains("<aboveturn id=\"cmp1\"/>"));
        assert!(!rendered.contains("large stale output\n<aboveturn"));
        drop(store);

        let reopened = Store::open(&path).unwrap();
        assert_eq!(
            reopened.visible_ids(&session).unwrap(),
            vec!["cmp1".to_owned(), "msg3".to_owned()]
        );
    }

    #[test]
    fn human_prompt_text_is_stored_verbatim_and_compaction_warns() {
        let temp = tempdir().unwrap();
        let mut store = Store::open(&temp.path().join("pcodx.sqlite3")).unwrap();
        let session = store
            .create_session(Some("ses-prompt"), temp.path())
            .unwrap();
        let prompt = "  exact human prompt\n\nkeep `quotes` and trailing spaces  ";
        let msg = store
            .record_message(&session, Role::User, prompt, Some("cli"))
            .unwrap();
        assert_eq!(store.messages(&session).unwrap()[0].text, prompt);
        let compaction = store
            .compact(&session, &msg.id, &msg.id, "summary")
            .unwrap();
        assert!(compaction.warning.unwrap().contains("human intent"));
        let rendered = store.render_visible_context(&session).unwrap();
        assert!(!rendered.contains(prompt));
        assert!(rendered.contains("summary"));
    }

    #[test]
    fn active_instruction_roles_can_be_compacted_with_warning() {
        let temp = tempdir().unwrap();
        let mut store = Store::open(&temp.path().join("pcodx.sqlite3")).unwrap();
        let session = store
            .create_session(Some("ses-instructions"), temp.path())
            .unwrap();
        let system = store
            .record_message(&session, Role::System, "keep system instruction", None)
            .unwrap();
        let developer = store
            .record_message(
                &session,
                Role::Developer,
                "keep developer instruction",
                None,
            )
            .unwrap();
        assert!(store
            .compact(&session, &system.id, &system.id, "summary")
            .unwrap()
            .warning
            .unwrap()
            .contains("system"));
        assert!(store
            .compact(&session, &developer.id, &developer.id, "summary")
            .unwrap()
            .warning
            .unwrap()
            .contains("system"));
    }

    #[test]
    fn visible_compaction_ids_can_be_range_boundaries() {
        let temp = tempdir().unwrap();
        let mut store = Store::open(&temp.path().join("pcodx.sqlite3")).unwrap();
        let session = store
            .create_session(Some("ses-cmp-boundary"), temp.path())
            .unwrap();
        store
            .record_message(&session, Role::Assistant, "old a", None)
            .unwrap();
        store
            .record_message(&session, Role::Assistant, "old b", None)
            .unwrap();
        store
            .record_message(&session, Role::Assistant, "old c", None)
            .unwrap();
        store.compact(&session, "msg1", "msg2", "old ab").unwrap();
        store.compact(&session, "cmp1", "msg3", "old abc").unwrap();
        assert_eq!(
            store.visible_ids(&session).unwrap(),
            vec!["cmp2".to_owned()]
        );
    }

    #[test]
    fn compaction_rejects_hidden_original_message_endpoints() {
        let temp = tempdir().unwrap();
        let mut store = Store::open(&temp.path().join("pcodx.sqlite3")).unwrap();
        let session = store
            .create_session(Some("ses-hidden-endpoint"), temp.path())
            .unwrap();
        store
            .record_message(&session, Role::Assistant, "old a", None)
            .unwrap();
        store
            .record_message(&session, Role::Assistant, "old b", None)
            .unwrap();
        store
            .record_message(&session, Role::Assistant, "new c", None)
            .unwrap();
        store.compact(&session, "msg1", "msg2", "old ab").unwrap();

        let err = store
            .compact(&session, "msg1", "msg3", "bad hidden endpoint")
            .unwrap_err();
        assert!(err.to_string().contains("not visible"));
        store
            .compact(&session, "cmp1", "msg3", "visible summary")
            .unwrap();
        assert_eq!(
            store.visible_ids(&session).unwrap(),
            vec!["cmp2".to_owned()]
        );
    }

    #[test]
    fn compaction_rejects_tool_pair_splits() {
        let temp = tempdir().unwrap();
        let mut store = Store::open(&temp.path().join("pcodx.sqlite3")).unwrap();
        let session = store
            .create_session(Some("ses-tool-pair"), temp.path())
            .unwrap();
        store
            .record_message(&session, Role::Assistant, "tool call", None)
            .unwrap();
        store
            .record_message(&session, Role::Tool, "tool result", None)
            .unwrap();
        store
            .record_message(&session, Role::Assistant, "final answer", None)
            .unwrap();

        let upper = store.compact(&session, "msg1", "msg1", "bad").unwrap_err();
        assert!(upper.to_string().contains("extend the range to msg2"));
        let lower = store.compact(&session, "msg2", "msg3", "bad").unwrap_err();
        assert!(lower
            .to_string()
            .contains("extend the range start back to msg1"));
        let compaction = store
            .compact(&session, "msg1", "msg2", "tool exchange summary")
            .unwrap();
        assert_eq!(compaction.id, "cmp1");
        assert_eq!(
            store.visible_ids(&session).unwrap(),
            vec!["cmp1".to_owned(), "msg3".to_owned()]
        );
    }

    #[test]
    fn compaction_rejects_multi_tool_result_group_splits() {
        let temp = tempdir().unwrap();
        let mut store = Store::open(&temp.path().join("pcodx.sqlite3")).unwrap();
        let session = store
            .create_session(Some("ses-tool-group"), temp.path())
            .unwrap();
        store
            .record_message(&session, Role::Assistant, "tool calls", None)
            .unwrap();
        store
            .record_message(&session, Role::Tool, "tool result one", None)
            .unwrap();
        store
            .record_message(&session, Role::Tool, "tool result two", None)
            .unwrap();
        store
            .record_message(&session, Role::Assistant, "final answer", None)
            .unwrap();

        let upper = store.compact(&session, "msg1", "msg2", "bad").unwrap_err();
        assert!(upper.to_string().contains("extend the range to msg3"));
        let lower = store.compact(&session, "msg3", "msg3", "bad").unwrap_err();
        assert!(lower
            .to_string()
            .contains("extend the range start back to msg1"));
        let compaction = store
            .compact(&session, "msg1", "msg3", "tool group summary")
            .unwrap();
        assert_eq!(compaction.id, "cmp1");
        assert_eq!(
            store.visible_ids(&session).unwrap(),
            vec!["cmp1".to_owned(), "msg4".to_owned()]
        );
    }

    #[test]
    fn current_session_message_id_lines_match_opencode_helper_shape() {
        let temp = tempdir().unwrap();
        let mut store = Store::open(&temp.path().join("pcodx.sqlite3")).unwrap();
        let session = store.create_session(Some("ses-ids"), temp.path()).unwrap();
        assert_eq!(
            store.current_session_message_id_lines(&session).unwrap(),
            "- No current-session message IDs are available yet."
        );
        for idx in 1..=18 {
            store
                .record_message(&session, Role::Assistant, &format!("message {idx}"), None)
                .unwrap();
        }
        store.compact(&session, "msg1", "msg2", "summary").unwrap();
        assert_eq!(
            store.current_session_message_id_lines(&session).unwrap(),
            "- cmp1, msg3, msg4, msg5, msg6, msg7, msg8, msg9\n- msg10, msg11, msg12, msg13, msg14, msg15, msg16, msg17\n- msg18"
        );
    }

    #[test]
    fn current_session_message_id_lines_truncate_like_opencode_helper() {
        let temp = tempdir().unwrap();
        let mut store = Store::open(&temp.path().join("pcodx.sqlite3")).unwrap();
        let session = store
            .create_session(Some("ses-ids-long"), temp.path())
            .unwrap();
        for idx in 1..=100 {
            store
                .record_message(&session, Role::Assistant, &format!("message {idx}"), None)
                .unwrap();
        }
        let lines = store.current_session_message_id_lines(&session).unwrap();
        assert!(lines.contains("- msg1, msg2, msg3, msg4, msg5, msg6, msg7, msg8"));
        assert!(lines.contains("- msg9, msg10, msg11, msg12, msg13, msg14, msg15, msg16"));
        assert!(lines.contains("- ... 4 older middle IDs omitted; use message search/read tools for current session history if you need them ..."));
        assert!(lines.contains("- msg21, msg22, msg23, msg24"));
        assert!(lines.contains("msg100"));
        assert!(!lines.contains("msg17"));
    }

    #[test]
    fn compact_ranges_prevalidates_and_writes_disjoint_ranges_atomically() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("pcodx.sqlite3");
        let mut store = Store::open(&path).unwrap();
        let session = store
            .create_session(Some("ses-batch"), temp.path())
            .unwrap();
        for text in [
            "old setup",
            "keep one",
            "old output",
            "old tool",
            "keep two",
        ] {
            store
                .record_message(&session, Role::Assistant, text, None)
                .unwrap();
        }

        let compactions = store
            .compact_ranges(
                &session,
                vec![
                    CompactionInput {
                        from_msg_id: "msg1".to_owned(),
                        to_msg_id: "msg1".to_owned(),
                        summary: "setup summary".to_owned(),
                    },
                    CompactionInput {
                        from_msg_id: "msg3".to_owned(),
                        to_msg_id: "msg4".to_owned(),
                        summary: "tool summary".to_owned(),
                    },
                ],
            )
            .unwrap();
        assert_eq!(compactions[0].id, "cmp1");
        assert_eq!(compactions[1].id, "cmp2");
        assert_eq!(
            store.visible_ids(&session).unwrap(),
            vec![
                "cmp1".to_owned(),
                "msg2".to_owned(),
                "cmp2".to_owned(),
                "msg5".to_owned()
            ]
        );

        let err = store
            .compact_ranges(
                &session,
                vec![
                    CompactionInput {
                        from_msg_id: "cmp1".to_owned(),
                        to_msg_id: "msg2".to_owned(),
                        summary: "bad overlap".to_owned(),
                    },
                    CompactionInput {
                        from_msg_id: "msg2".to_owned(),
                        to_msg_id: "cmp2".to_owned(),
                        summary: "bad overlap two".to_owned(),
                    },
                ],
            )
            .unwrap_err();
        assert!(err.to_string().contains("overlaps another requested range"));
        let reopened = Store::open(&path).unwrap();
        assert_eq!(
            reopened.visible_ids(&session).unwrap(),
            vec![
                "cmp1".to_owned(),
                "msg2".to_owned(),
                "cmp2".to_owned(),
                "msg5".to_owned()
            ]
        );
    }

    #[test]
    fn migrates_first_skeleton_schema_and_padded_ids() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("pcodx.sqlite3");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "
            CREATE TABLE sessions(
              id TEXT PRIMARY KEY,
              cwd TEXT NOT NULL,
              created_at_ms INTEGER NOT NULL,
              updated_at_ms INTEGER NOT NULL,
              upstream_session_id TEXT,
              kv_cache_boundary TEXT NOT NULL DEFAULT 'future_turn_only'
            );
            CREATE TABLE messages(
              id TEXT NOT NULL,
              session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
              seq INTEGER NOT NULL,
              role TEXT NOT NULL CHECK(role IN ('system', 'developer', 'user', 'assistant', 'tool')),
              text TEXT NOT NULL,
              source TEXT,
              created_at_ms INTEGER NOT NULL,
              is_human_prompt INTEGER NOT NULL CHECK(is_human_prompt IN (0, 1)),
              must_preserve INTEGER NOT NULL CHECK(must_preserve IN (0, 1)),
              PRIMARY KEY(session_id, id),
              UNIQUE(session_id, seq)
            );
            CREATE TABLE compactions(
              id TEXT NOT NULL,
              session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
              seq INTEGER NOT NULL,
              from_msg_id TEXT NOT NULL,
              to_msg_id TEXT NOT NULL,
              from_seq INTEGER NOT NULL,
              to_seq INTEGER NOT NULL,
              summary TEXT NOT NULL,
              created_at_ms INTEGER NOT NULL,
              n_messages_replaced INTEGER NOT NULL,
              PRIMARY KEY(session_id, id),
              UNIQUE(session_id, seq),
              CHECK(from_seq <= to_seq)
            );
            ",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO sessions(id, cwd, created_at_ms, updated_at_ms) VALUES (?1, ?2, 1, 1)",
            params!["old", temp.path().display().to_string()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO messages(id, session_id, seq, role, text, source, created_at_ms, is_human_prompt, must_preserve)
             VALUES ('msg000001', 'old', 1, 'user', 'old prompt', NULL, 1, 1, 1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO messages(id, session_id, seq, role, text, source, created_at_ms, is_human_prompt, must_preserve)
             VALUES ('msg000002', 'old', 2, 'assistant', 'old assistant', NULL, 1, 0, 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO compactions(id, session_id, seq, from_msg_id, to_msg_id, from_seq, to_seq, summary, created_at_ms, n_messages_replaced)
             VALUES ('cmp000001', 'old', 1, 'msg000002', 'msg000002', 2, 2, 'old compacted assistant', 1, 1)",
            [],
        )
        .unwrap();
        drop(conn);

        let mut store = Store::open(&path).unwrap();
        let messages = store.messages("old").unwrap();
        assert_eq!(messages[0].id, "msg1");
        assert_eq!(messages[1].id, "msg2");
        assert!(messages[0].preserve_warning);
        assert_eq!(
            store.visible_ids("old").unwrap(),
            vec!["msg1".to_owned(), "cmp1".to_owned()]
        );
        let new_message = store
            .record_message("old", Role::Assistant, "new assistant", None)
            .unwrap();
        assert_eq!(new_message.id, "msg3");
        let compaction = store.compact("old", "msg1", "cmp1", "summary").unwrap();
        assert!(compaction.warning.unwrap().contains("human intent"));
        assert_eq!(
            store.visible_ids("old").unwrap(),
            vec!["cmp2".to_owned(), "msg3".to_owned()]
        );
    }
}
