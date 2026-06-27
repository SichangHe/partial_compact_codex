use rusqlite::{params, Connection, OptionalExtension};
use std::env;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::{SystemTime, UNIX_EPOCH};

pub type Result<T> = std::result::Result<T, Error>;

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
    pub must_preserve: bool,
}

#[derive(Debug)]
pub struct Compaction {
    pub id: String,
    pub from_msg_id: String,
    pub to_msg_id: String,
    pub summary: String,
    pub n_messages_replaced: i64,
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
        Ok(store)
    }

    pub fn default_path() -> PathBuf {
        if let Some(path) = env::var_os("PCODX_DB") {
            return PathBuf::from(path);
        }
        if let Some(path) = env::var_os("XDG_DATA_HOME") {
            return PathBuf::from(path).join("partial_compact_codex/pcodx.sqlite3");
        }
        let home = env::var_os("HOME").unwrap_or_else(|| ".".into());
        PathBuf::from(home).join(".local/share/partial_compact_codex/pcodx.sqlite3")
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
        let must_preserve = matches!(role, Role::System | Role::Developer | Role::User);
        tx.execute(
            "INSERT INTO messages(id, session_id, seq, role, text, source, created_at_ms, is_human_prompt, must_preserve)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![id, session_id, seq, role.as_str(), text, source, now_ms, is_human_prompt, must_preserve],
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
            must_preserve,
        })
    }

    pub fn messages(&self, session_id: &str) -> Result<Vec<Message>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, seq, role, text, source, is_human_prompt, must_preserve
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
        if summary.trim().is_empty() {
            return Err(Error::Invalid("summary must be non-empty".to_owned()));
        }
        self.ensure_session(session_id)?;
        let from_seq = self.message_seq(session_id, from_msg_id)?;
        let to_seq = self.message_seq(session_id, to_msg_id)?;
        if from_seq > to_seq {
            return Err(Error::Invalid(format!(
                "{from_msg_id} comes after {to_msg_id}"
            )));
        }
        let n_preserved: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM messages
             WHERE session_id = ?1 AND seq BETWEEN ?2 AND ?3 AND must_preserve = 1",
            params![session_id, from_seq, to_seq],
            |row| row.get(0),
        )?;
        if n_preserved > 0 {
            return Err(Error::Invalid(
                "range contains a preserved instruction or human prompt".to_owned(),
            ));
        }
        let n_overlaps: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM compactions
             WHERE session_id = ?1 AND NOT (to_seq < ?2 OR from_seq > ?3)",
            params![session_id, from_seq, to_seq],
            |row| row.get(0),
        )?;
        if n_overlaps > 0 {
            return Err(Error::Invalid(
                "range overlaps an existing compaction".to_owned(),
            ));
        }
        let tx = self.conn.transaction()?;
        let seq: i64 = tx.query_row(
            "SELECT COALESCE(MAX(seq), 0) + 1 FROM compactions WHERE session_id = ?1",
            params![session_id],
            |row| row.get(0),
        )?;
        let id = format_cmp_id(seq);
        let now_ms = now_unix_ms();
        let n_messages_replaced = to_seq - from_seq + 1;
        tx.execute(
            "INSERT INTO compactions(id, session_id, seq, from_msg_id, to_msg_id, from_seq, to_seq, summary, created_at_ms, n_messages_replaced)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![id, session_id, seq, from_msg_id, to_msg_id, from_seq, to_seq, summary, now_ms, n_messages_replaced],
        )?;
        touch_session_tx(&tx, session_id, now_ms)?;
        tx.commit()?;
        Ok(Compaction {
            id,
            from_msg_id: from_msg_id.to_owned(),
            to_msg_id: to_msg_id.to_owned(),
            summary: summary.to_owned(),
            n_messages_replaced,
        })
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
                    must_preserve: message.must_preserve,
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

    pub fn render_visible_context(&self, session_id: &str) -> Result<String> {
        let mut blocks = vec![format!(
            "pcodx session `{session_id}` compacted future-turn context.\n\
             This render is for a fresh future Codex turn; do not mutate a live hidden transcript in place."
        )];
        for entry in self.visible_entries(session_id)? {
            match entry {
                VisibleEntry::Message(message) => blocks.push(format!(
                    "{}\n<pcodx-message id=\"{}\" role=\"{}\" />",
                    message.text,
                    message.id,
                    message.role.as_str()
                )),
                VisibleEntry::Compaction(compaction) => blocks.push(format!(
                    "{}\n<pcodx-compacted id=\"{}\" range=\"{}..{}\" />",
                    compaction.summary, compaction.id, compaction.from_msg_id, compaction.to_msg_id
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
              must_preserve INTEGER NOT NULL CHECK(must_preserve IN (0, 1)),
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
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Error::from)
    }
}

fn message_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Message> {
    let role_text: String = row.get(2)?;
    let role = Role::from_str(&role_text).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(2, rusqlite::types::Type::Text, Box::new(error))
    })?;
    let is_human_prompt: i64 = row.get(5)?;
    let must_preserve: i64 = row.get(6)?;
    Ok(Message {
        id: row.get(0)?,
        seq: row.get(1)?,
        role,
        text: row.get(3)?,
        source: row.get(4)?,
        is_human_prompt: is_human_prompt != 0,
        must_preserve: must_preserve != 0,
    })
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
    format!("msg{seq:06}")
}

fn format_cmp_id(seq: i64) -> String {
    format!("cmp{seq:06}")
}

fn parse_msg_seq(msg_id: &str) -> Result<i64> {
    let suffix = msg_id
        .strip_prefix("msg")
        .ok_or_else(|| Error::Invalid(format!("invalid message id `{msg_id}`")))?;
    if suffix.len() != 6 || !suffix.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(Error::Invalid(format!("invalid message id `{msg_id}`")));
    }
    suffix
        .parse::<i64>()
        .map_err(|_| Error::Invalid(format!("invalid message id `{msg_id}`")))
}

#[cfg(test)]
mod tests {
    use super::{Role, Store};
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
            .compact(&session, &stale.id, "msg000002", "summary of stale work")
            .unwrap();
        assert_eq!(compaction.id, "cmp000001");
        assert_eq!(
            store.visible_ids(&session).unwrap(),
            vec!["cmp000001".to_owned(), kept.id]
        );
        let rendered = store.render_visible_context(&session).unwrap();
        assert!(rendered.contains("summary of stale work"));
        assert!(!rendered.contains("large stale output\n<pcodx-message"));
        drop(store);

        let reopened = Store::open(&path).unwrap();
        assert_eq!(
            reopened.visible_ids(&session).unwrap(),
            vec!["cmp000001".to_owned(), "msg000003".to_owned()]
        );
    }

    #[test]
    fn human_prompt_text_is_stored_verbatim_and_not_compacted() {
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
        let error = store
            .compact(&session, &msg.id, &msg.id, "summary")
            .unwrap_err()
            .to_string();
        assert!(error.contains("preserved"));
        let rendered = store.render_visible_context(&session).unwrap();
        assert!(rendered.contains(prompt));
    }

    #[test]
    fn active_instruction_roles_are_not_compacted() {
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
            .unwrap_err()
            .to_string()
            .contains("preserved"));
        assert!(store
            .compact(&session, &developer.id, &developer.id, "summary")
            .unwrap_err()
            .to_string()
            .contains("preserved"));
    }
}
