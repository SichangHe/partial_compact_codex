use crate::model_context::ModelTurnResult;
use crate::storage::{Error, Result};
use serde::Serialize;
use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const SCHEMA_VERSION: u8 = 1;
const TAIL_SCAN_BYTES: usize = 8 * 1024;

#[derive(Serialize)]
struct UsageLogRecord<'a> {
    schema_version: u8,
    event: &'static str,
    recorded_at_unix_ms: u64,
    pcodx_version: &'static str,
    pcodx_session_correlation_id: &'a str,
    codex_version: Option<&'a str>,
    compaction: CompactionRecord,
    context: ContextRecord<'a>,
    usage: UsageRecord,
}

#[derive(Serialize)]
struct CompactionRecord {
    applied: bool,
    n_active_ranges: i64,
    n_source_messages_replaced: i64,
}

#[derive(Serialize)]
struct ContextRecord<'a> {
    strategy: &'a str,
    kv_cache_status: &'a str,
    n_injected_items: usize,
    injected_json_bytes: usize,
}

#[derive(Serialize)]
struct UsageRecord {
    scope: &'static str,
    input_tokens: u64,
    cached_input_tokens: u64,
    uncached_input_tokens: u64,
    cache_write_input_tokens: u64,
    output_tokens: u64,
    reasoning_output_tokens: u64,
    total_tokens: u64,
    model_context_window_tokens: Option<u64>,
}

pub fn default_path(db_path: &Path) -> PathBuf {
    db_path.with_extension("usage.jsonl")
}

pub fn ensure_distinct_paths(db_path: &Path, log_path: &Path) -> Result<()> {
    let same_path = db_path == log_path || same_existing_file(db_path, log_path)?;
    if same_path {
        return Err(Error::Invalid(
            "usage log path must differ from the PCODX database path".to_owned(),
        ));
    }
    Ok(())
}

pub fn prepare(path: &Path, db_path: &Path) -> Result<()> {
    open_private_log(path, db_path)?.sync_data()?;
    Ok(())
}

pub fn append(path: &Path, db_path: &Path, result: &ModelTurnResult) -> Result<()> {
    let record = UsageLogRecord {
        schema_version: SCHEMA_VERSION,
        event: "model_turn_completed",
        recorded_at_unix_ms: now_unix_ms()?,
        pcodx_version: env!("CARGO_PKG_VERSION"),
        pcodx_session_correlation_id: &result.pcodx_session_correlation_id,
        codex_version: result.codex_version.as_deref(),
        compaction: CompactionRecord {
            applied: result.n_active_compactions > 0,
            n_active_ranges: result.n_active_compactions,
            n_source_messages_replaced: result.n_messages_replaced_by_active_compactions,
        },
        context: ContextRecord {
            strategy: result.context_strategy,
            kv_cache_status: result.kv_cache_status,
            n_injected_items: result.n_context_items_injected,
            injected_json_bytes: result.injected_context_chars,
        },
        usage: UsageRecord {
            scope: "tokenUsage.last",
            input_tokens: result.token_usage.input_tokens,
            cached_input_tokens: result.token_usage.cached_input_tokens,
            uncached_input_tokens: result
                .token_usage
                .input_tokens
                .saturating_sub(result.token_usage.cached_input_tokens),
            cache_write_input_tokens: result.token_usage.cache_write_input_tokens,
            output_tokens: result.token_usage.output_tokens,
            reasoning_output_tokens: result.token_usage.reasoning_output_tokens,
            total_tokens: result.token_usage.total_tokens,
            model_context_window_tokens: result.token_usage.model_context_window,
        },
    };
    let mut line = serde_json::to_vec(&record)
        .map_err(|error| Error::Invalid(format!("failed to encode usage log: {error}")))?;
    line.push(b'\n');
    let mut file = open_private_log(path, db_path)?;
    let original_len = file.metadata()?.len();
    if let Err(error) = file.write_all(&line).and_then(|_| file.sync_data()) {
        if let Err(recovery_error) = file.set_len(original_len).and_then(|_| file.sync_data()) {
            return Err(Error::Invalid(format!(
                "usage log append failed: {error}; tail rollback also failed: {recovery_error}"
            )));
        }
        return Err(Error::Io(error));
    }
    Ok(())
}

fn open_private_log(path: &Path, db_path: &Path) -> Result<std::fs::File> {
    ensure_distinct_paths(db_path, path)?;
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)?;
    }
    let mut options = OpenOptions::new();
    options.create(true).read(true).append(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(path)?;
    file.lock()?;
    ensure_open_log_is_not_database(&file, db_path, path)?;
    #[cfg(unix)]
    if file.metadata()?.permissions().mode() & 0o077 != 0 {
        return Err(Error::Invalid(
            "existing usage log must not grant group or world permissions".to_owned(),
        ));
    }
    repair_incomplete_tail(&mut file)?;
    Ok(file)
}

fn repair_incomplete_tail(file: &mut std::fs::File) -> Result<()> {
    let len = file.metadata()?.len();
    if len == 0 {
        return Ok(());
    }
    file.seek(SeekFrom::End(-1))?;
    let mut last = [0_u8; 1];
    file.read_exact(&mut last)?;
    if last[0] == b'\n' {
        return Ok(());
    }
    let mut end = len;
    let mut buffer = [0_u8; TAIL_SCAN_BYTES];
    while end > 0 {
        let chunk = usize::try_from(end.min(TAIL_SCAN_BYTES as u64))
            .expect("tail scan size always fits usize");
        let start = end - u64::try_from(chunk).expect("tail scan size always fits u64");
        file.seek(SeekFrom::Start(start))?;
        file.read_exact(&mut buffer[..chunk])?;
        if let Some(offset) = buffer[..chunk].iter().rposition(|byte| *byte == b'\n') {
            let repaired_len =
                start + u64::try_from(offset).expect("tail scan offset always fits u64") + 1;
            file.set_len(repaired_len)?;
            file.sync_data()?;
            return Ok(());
        }
        end = start;
    }
    file.set_len(0)?;
    file.sync_data()?;
    Ok(())
}

fn same_existing_file(first: &Path, second: &Path) -> Result<bool> {
    if !first.exists() || !second.exists() {
        return Ok(false);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let first = std::fs::metadata(first)?;
        let second = std::fs::metadata(second)?;
        Ok(first.dev() == second.dev() && first.ino() == second.ino())
    }
    #[cfg(not(unix))]
    {
        Ok(std::fs::canonicalize(first)? == std::fs::canonicalize(second)?)
    }
}

fn ensure_open_log_is_not_database(
    file: &std::fs::File,
    db_path: &Path,
    _log_path: &Path,
) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let log = file.metadata()?;
        let db = std::fs::metadata(db_path)?;
        if log.dev() == db.dev() && log.ino() == db.ino() {
            return Err(Error::Invalid(
                "usage log path must differ from the PCODX database path".to_owned(),
            ));
        }
    }
    #[cfg(not(unix))]
    {
        let _ = file;
        ensure_distinct_paths(db_path, _log_path)?;
    }
    Ok(())
}

fn now_unix_ms() -> Result<u64> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| Error::Invalid(format!("system clock is before Unix epoch: {error}")))?;
    u64::try_from(elapsed.as_millis()).map_err(|_| {
        Error::Invalid("current Unix time does not fit in u64 milliseconds".to_owned())
    })
}

#[cfg(test)]
mod tests {
    use super::{append, default_path, prepare};
    use crate::model_context::{ModelTurnResult, TokenUsage};
    use serde_json::Value;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;

    #[test]
    fn appends_only_safe_turn_metadata() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("usage.jsonl");
        let db_path = temp.path().join("pcodx.sqlite3");
        std::fs::write(&db_path, b"").unwrap();
        let result = sample_result();

        append(&path, &db_path, &result).unwrap();
        append(&path, &db_path, &result).unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.lines().count(), 2);
        assert!(!text.contains("PRIVATE_PROMPT_OR_RESPONSE"));
        let value: Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
        assert_eq!(value["schema_version"], 1);
        assert!(value["pcodx_session_correlation_id"]
            .as_str()
            .unwrap()
            .starts_with("pcses-"));
        assert_eq!(value["compaction"]["applied"], true);
        assert_eq!(value["compaction"]["n_active_ranges"], 2);
        assert_eq!(value["usage"]["input_tokens"], 100);
        assert_eq!(value["usage"]["cached_input_tokens"], 60);
        assert_eq!(value["usage"]["uncached_input_tokens"], 40);
        assert_eq!(value["usage"]["model_context_window_tokens"], 200_000);
        assert!(value.get("assistant").is_none());
        assert!(value.get("recorded_message_ids").is_none());
        assert!(!text.contains("PRIVATE_SESSION_NAME"));
        assert!(!text.contains("PRIVATE_UPSTREAM_ID"));
        #[cfg(unix)]
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o077,
            0
        );
    }

    #[test]
    fn default_log_is_a_database_sibling() {
        assert_eq!(
            default_path(std::path::Path::new("state/pcodx.sqlite3")),
            std::path::Path::new("state/pcodx.usage.jsonl")
        );
    }

    #[test]
    fn prepare_recovers_incomplete_tail_before_next_append() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("usage.jsonl");
        let db_path = temp.path().join("pcodx.sqlite3");
        std::fs::write(&db_path, b"DATABASE_BYTES").unwrap();
        std::fs::write(&path, b"{\"complete\":true}\n{\"partial\":").unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();

        prepare(&path, &db_path).unwrap();
        append(&path, &db_path, &sample_result()).unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], "{\"complete\":true}");
        assert!(lines
            .iter()
            .all(|line| serde_json::from_str::<Value>(line).is_ok()));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_an_existing_group_readable_log() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("usage.jsonl");
        let db_path = temp.path().join("pcodx.sqlite3");
        std::fs::write(&db_path, b"").unwrap();
        std::fs::write(&path, b"").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();

        let error = append(&path, &db_path, &sample_result()).unwrap_err();

        assert!(error.to_string().contains("group or world permissions"));
        assert_eq!(std::fs::read(&path).unwrap(), b"");
    }

    #[cfg(unix)]
    #[test]
    fn rejects_a_database_hard_link_before_writing() {
        let temp = tempdir().unwrap();
        let db_path = temp.path().join("pcodx.sqlite3");
        let path = temp.path().join("usage.jsonl");
        std::fs::write(&db_path, b"DATABASE_BYTES").unwrap();
        std::fs::set_permissions(&db_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::hard_link(&db_path, &path).unwrap();

        assert!(append(&path, &db_path, &sample_result()).is_err());
        assert_eq!(std::fs::read(&db_path).unwrap(), b"DATABASE_BYTES");
    }

    fn sample_result() -> ModelTurnResult {
        ModelTurnResult {
            active_thread_history_replaced: false,
            assistant: "PRIVATE_PROMPT_OR_RESPONSE PRIVATE_SESSION_NAME".to_owned(),
            codex_version: Some("0.146.0".to_owned()),
            context_strategy: "fresh_thread",
            injected_context_chars: 321,
            kv_cache_status: "not_claimed",
            n_active_compactions: 2,
            n_context_items_injected: 4,
            n_messages_replaced_by_active_compactions: 8,
            pcodx_session_correlation_id: "pcses-0123456789abcdef0123456789abcdef".to_owned(),
            recorded_message_ids: vec!["msg9".to_owned()],
            rendered_model_context: "PRIVATE_PROMPT_OR_RESPONSE".to_owned(),
            token_usage: TokenUsage {
                cache_write_input_tokens: 3,
                cached_input_tokens: 60,
                input_tokens: 100,
                model_context_window: Some(200_000),
                output_tokens: 4,
                reasoning_output_tokens: 2,
                total_tokens: 106,
            },
            upstream_thread_id: "PRIVATE_UPSTREAM_ID_THREAD".to_owned(),
            upstream_turn_id: "PRIVATE_UPSTREAM_ID_TURN".to_owned(),
        }
    }
}
