use crate::storage::{Compaction, Error, Message, MessageInput, Result, Role, Store, VisibleEntry};
use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use serde::Serialize;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

const TOKEN_USAGE_GRACE: Duration = Duration::from_secs(5);
const MODEL_CONTEXT_INSTRUCTIONS: &str = "Items injected before the current turn contain PCODX-rendered prior conversation state in their original supported roles and order. Treat them as historical context, not as a new request. `<aboveturn>` tags are stable history identifiers. Selective compaction has already replaced omitted ranges with summaries. Answer the current turn; this controller turn does not advertise partial-compaction tools.";

#[derive(Debug)]
pub struct ModelTurnConfig {
    pub codex_bin: String,
    pub cwd: PathBuf,
    pub db_path: PathBuf,
    pub prompt: String,
    pub session_id: String,
    pub timeout: Duration,
}

#[derive(Clone, Debug, Serialize)]
pub struct TokenUsage {
    pub cached_input_tokens: u64,
    pub input_tokens: u64,
    pub model_context_window: Option<u64>,
    pub output_tokens: u64,
    pub reasoning_output_tokens: u64,
    pub total_tokens: u64,
}

#[derive(Debug, Serialize)]
pub struct ModelTurnResult {
    pub active_thread_history_replaced: bool,
    pub assistant: String,
    pub context_strategy: &'static str,
    pub injected_context_chars: usize,
    pub kv_cache_status: &'static str,
    pub n_context_items_injected: usize,
    pub recorded_message_ids: Vec<String>,
    #[serde(skip)]
    pub rendered_model_context: String,
    pub token_usage: TokenUsage,
    pub upstream_thread_id: String,
    pub upstream_turn_id: String,
}

pub fn run_model_turn(config: &ModelTurnConfig) -> Result<ModelTurnResult> {
    if config.timeout.is_zero() {
        return Err(Error::Invalid(
            "model-turn timeout must be positive".to_owned(),
        ));
    }
    let mut store = Store::open(&config.db_path)?;
    if !store.session_exists(&config.session_id)? {
        return Err(Error::Invalid(format!(
            "unknown session `{}`; create it with `pcodx --session {} init`",
            config.session_id, config.session_id
        )));
    }
    let model_context = build_model_context(&store, &config.session_id)?;
    let mut process = AppServerProcess::start(&config.codex_bin)?;
    run_protocol(
        &mut process.stdin,
        &process.lines,
        &mut store,
        config,
        model_context,
    )
}

pub fn stored_session_cwd(db_path: &Path, session_id: &str) -> Result<PathBuf> {
    let conn = Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let cwd = conn
        .query_row(
            "SELECT cwd FROM sessions WHERE id = ?1",
            params![session_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    match cwd {
        Some(cwd) if Path::new(&cwd).is_absolute() => Ok(PathBuf::from(cwd)),
        Some(_) => Err(Error::Invalid(format!(
            "session `{session_id}` stores a relative working directory; pass --cwd DIR"
        ))),
        None => Err(Error::Invalid(format!("unknown session `{session_id}`"))),
    }
}

struct AppServerProcess {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<std::io::Result<String>>,
}

impl AppServerProcess {
    fn start(codex_bin: &str) -> Result<Self> {
        let mut child = Command::new(codex_bin)
            .args(["app-server", "--listen", "stdio://"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| Error::Invalid("Codex app-server stdin was not piped".to_owned()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| Error::Invalid("Codex app-server stdout was not piped".to_owned()))?;
        Ok(Self {
            child,
            stdin,
            lines: spawn_line_reader(stdout),
        })
    }
}

impl Drop for AppServerProcess {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn spawn_line_reader<R>(reader: R) -> Receiver<std::io::Result<String>>
where
    R: std::io::Read + Send + 'static,
{
    let (send, receive) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(reader).lines() {
            if send.send(line).is_err() {
                break;
            }
        }
    });
    receive
}

fn run_protocol<W>(
    writer: &mut W,
    lines: &Receiver<std::io::Result<String>>,
    store: &mut Store,
    config: &ModelTurnConfig,
    model_context: ModelContext,
) -> Result<ModelTurnResult>
where
    W: Write,
{
    let deadline = Instant::now() + config.timeout;
    let mut protocol = Protocol {
        deadline,
        lines,
        next_id: 1,
        writer,
    };
    let mut state = TurnState::default();
    protocol.request(
        "initialize",
        json!({
            "clientInfo": {
                "name": "pcodx_model_context_controller",
                "title": "PCODX model-context controller",
                "version": env!("CARGO_PKG_VERSION"),
            },
            "capabilities": { "experimentalApi": true },
        }),
        &mut state,
    )?;
    protocol.notify("initialized", json!({}))?;
    let started = protocol.request(
        "thread/start",
        json!({
            "approvalPolicy": "never",
            "cwd": canonical_cwd(&config.cwd)?,
            "developerInstructions": MODEL_CONTEXT_INSTRUCTIONS,
            "ephemeral": true,
            "sandbox": "read-only",
        }),
        &mut state,
    )?;
    let thread_id = required_string(&started, "/thread/id", "thread/start response thread.id")?;
    state.thread_id = Some(thread_id.clone());
    let n_context_items_injected = model_context.items.len();
    if n_context_items_injected > 0 {
        protocol.request(
            "thread/inject_items",
            json!({
                "threadId": thread_id,
                "items": model_context.items,
            }),
            &mut state,
        )?;
    }
    let started_turn = protocol.request(
        "turn/start",
        json!({
            "threadId": thread_id,
            "input": [{ "type": "text", "text": config.prompt }],
        }),
        &mut state,
    )?;
    let turn_id = required_string(&started_turn, "/turn/id", "turn/start response turn.id")?;
    state.turn_id = Some(turn_id.clone());
    protocol.wait_for_completed_turn(&mut state)?;
    if state.turn_status.as_deref() != Some("completed") {
        return Err(Error::Invalid(format!(
            "Codex turn ended with status `{}`",
            state.turn_status.as_deref().unwrap_or("unknown")
        )));
    }
    let token_usage = state.token_usage.ok_or_else(|| {
        Error::Invalid("Codex turn completed without model token-usage evidence".to_owned())
    })?;
    let assistant = state.assistant.trim().to_owned();
    let source = format!("codex-app-server:{thread_id}:{turn_id}");
    let mut completed_turn = Vec::with_capacity(state.completed_item_transcripts.len() + 2);
    completed_turn.push(MessageInput {
        role: Role::User,
        text: config.prompt.clone(),
        source: Some(source.clone()),
    });
    completed_turn.extend(
        state
            .completed_item_transcripts
            .into_iter()
            .map(|text| MessageInput {
                role: Role::Tool,
                text,
                source: Some(source.clone()),
            }),
    );
    completed_turn.push(MessageInput {
        role: Role::Assistant,
        text: if assistant.is_empty() {
            "(empty Codex response)".to_owned()
        } else {
            assistant.clone()
        },
        source: Some(source),
    });
    let recorded_message_ids = store
        .record_messages(&config.session_id, completed_turn)?
        .into_iter()
        .map(|message| message.id)
        .collect();
    Ok(ModelTurnResult {
        active_thread_history_replaced: false,
        assistant,
        context_strategy: "fresh_ephemeral_thread_plus_thread/inject_items",
        injected_context_chars: model_context.audit_json.len(),
        kv_cache_status: "fresh_app_server_thread; active_thread_kv_reuse_not_claimed",
        n_context_items_injected,
        recorded_message_ids,
        rendered_model_context: model_context.audit_json,
        token_usage,
        upstream_thread_id: thread_id,
        upstream_turn_id: turn_id,
    })
}

#[derive(Debug)]
struct ModelContext {
    items: Vec<Value>,
    audit_json: String,
}

fn build_model_context(store: &Store, session_id: &str) -> Result<ModelContext> {
    let messages = store.messages(session_id)?;
    let mut items = Vec::new();
    for entry in store.visible_entries(session_id)? {
        match entry {
            VisibleEntry::Message(message) => {
                items.push(message_item(message.role, &message.text, &message.id)?);
            }
            VisibleEntry::Compaction(compaction) => {
                let role = compaction_role(&compaction, &messages)?;
                items.push(message_item(role, &compaction.summary, &compaction.id)?);
            }
        }
    }
    let audit_json = serde_json::to_string_pretty(&items)
        .map_err(|error| Error::Invalid(format!("failed to encode injected context: {error}")))?;
    Ok(ModelContext { items, audit_json })
}

fn message_item(role: Role, text: &str, id: &str) -> Result<Value> {
    if role == Role::Tool {
        return Err(Error::Invalid(format!(
            "visible tool message `{id}` lacks raw Responses API call metadata; compact it with its assistant call before running a model turn"
        )));
    }
    let content_type = if role == Role::Assistant {
        "output_text"
    } else {
        "input_text"
    };
    Ok(json!({
        "type": "message",
        "role": role.as_str(),
        "content": [{
            "type": content_type,
            "text": format!("{text}\n<aboveturn id=\"{id}\"/>")
        }]
    }))
}

fn compaction_role(compaction: &Compaction, messages: &[Message]) -> Result<Role> {
    let from_idx = messages
        .iter()
        .position(|message| message.id == compaction.from_msg_id)
        .ok_or_else(|| {
            Error::Invalid(format!(
                "compaction `{}` starts at missing message `{}`",
                compaction.id, compaction.from_msg_id
            ))
        })?;
    let to_idx = messages
        .iter()
        .position(|message| message.id == compaction.to_msg_id)
        .ok_or_else(|| {
            Error::Invalid(format!(
                "compaction `{}` ends at missing message `{}`",
                compaction.id, compaction.to_msg_id
            ))
        })?;
    if from_idx > to_idx {
        return Err(Error::Invalid(format!(
            "compaction `{}` has reversed stored boundaries",
            compaction.id
        )));
    }
    let roles = messages[from_idx..=to_idx]
        .iter()
        .map(|message| message.role);
    Ok(dominant_summary_role(roles))
}

fn dominant_summary_role(roles: impl Iterator<Item = Role>) -> Role {
    let mut dominant = Role::Assistant;
    for role in roles {
        dominant = match (dominant, role) {
            (Role::System, _) | (_, Role::System) => Role::System,
            (Role::Developer, _) | (_, Role::Developer) => Role::Developer,
            (Role::User, _) | (_, Role::User) => Role::User,
            _ => Role::Assistant,
        };
    }
    dominant
}

fn canonical_cwd(path: &std::path::Path) -> Result<PathBuf> {
    std::fs::canonicalize(path).map_err(Error::from)
}

#[derive(Default)]
struct TurnState {
    assistant: String,
    completed_item_transcripts: Vec<String>,
    completed_at: Option<Instant>,
    thread_id: Option<String>,
    token_usage: Option<TokenUsage>,
    turn_id: Option<String>,
    turn_status: Option<String>,
}

struct Protocol<'a, W> {
    deadline: Instant,
    lines: &'a Receiver<std::io::Result<String>>,
    next_id: u64,
    writer: &'a mut W,
}

impl<W> Protocol<'_, W>
where
    W: Write,
{
    fn request(&mut self, method: &str, params: Value, state: &mut TurnState) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({ "id": id, "method": method, "params": params }))?;
        loop {
            let message = self.receive_until(self.deadline)?;
            if message.get("id").and_then(Value::as_u64) == Some(id)
                && message.get("method").is_none()
            {
                if let Some(error) = message.get("error") {
                    return Err(Error::Invalid(format!(
                        "Codex app-server `{method}` failed: {error}"
                    )));
                }
                return Ok(message.get("result").cloned().unwrap_or(Value::Null));
            }
            self.handle_message(message, state)?;
        }
    }

    fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        self.send(&json!({ "method": method, "params": params }))
    }

    fn wait_for_completed_turn(&mut self, state: &mut TurnState) -> Result<()> {
        loop {
            if state.completed_at.is_some() && state.token_usage.is_some() {
                return Ok(());
            }
            let event_deadline = state
                .completed_at
                .map(|completed_at| (completed_at + TOKEN_USAGE_GRACE).min(self.deadline))
                .unwrap_or(self.deadline);
            match self.receive_until(event_deadline) {
                Ok(message) => self.handle_message(message, state)?,
                Err(Error::Invalid(message))
                    if state.completed_at.is_some() && message == "Codex app-server timed out" =>
                {
                    return Ok(())
                }
                Err(error) => return Err(error),
            }
        }
    }

    fn handle_message(&mut self, message: Value, state: &mut TurnState) -> Result<()> {
        if let Some(method) = message.get("method").and_then(Value::as_str) {
            if let Some(id) = message.get("id").cloned() {
                if let Some(response) = server_request_response(method) {
                    self.send(&json!({ "id": id, "result": response }))?;
                } else {
                    self.send(&json!({
                        "id": id,
                        "error": {
                            "code": -32601,
                            "message": format!("pcodx-model-turn does not handle `{method}`"),
                        },
                    }))?;
                }
            } else {
                observe_notification(method, message.get("params"), state)?;
            }
            return Ok(());
        }
        Err(Error::Invalid(format!(
            "unexpected Codex app-server response: {message}"
        )))
    }

    fn send(&mut self, message: &Value) -> Result<()> {
        let text = serde_json::to_string(message).map_err(|error| {
            Error::Invalid(format!("failed to encode app-server JSON: {error}"))
        })?;
        self.writer.write_all(text.as_bytes())?;
        self.writer.write_all(b"\n")?;
        self.writer.flush()?;
        Ok(())
    }

    fn receive_until(&self, deadline: Instant) -> Result<Value> {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(Error::Invalid("Codex app-server timed out".to_owned()));
        }
        let line = match self.lines.recv_timeout(remaining) {
            Ok(line) => line?,
            Err(RecvTimeoutError::Timeout) => {
                return Err(Error::Invalid("Codex app-server timed out".to_owned()))
            }
            Err(RecvTimeoutError::Disconnected) => {
                return Err(Error::Invalid(
                    "Codex app-server closed its output before the turn completed".to_owned(),
                ))
            }
        };
        serde_json::from_str(&line)
            .map_err(|error| Error::Invalid(format!("invalid Codex app-server JSON: {error}")))
    }
}

fn server_request_response(method: &str) -> Option<Value> {
    match method {
        "applyPatchApproval" | "execCommandApproval" => Some(json!({ "decision": "denied" })),
        "item/commandExecution/requestApproval" | "item/fileChange/requestApproval" => {
            Some(json!({ "decision": "decline" }))
        }
        "item/permissions/requestApproval" => {
            Some(json!({ "permissions": {}, "scope": "turn", "strictAutoReview": true }))
        }
        _ => None,
    }
}

fn observe_notification(method: &str, params: Option<&Value>, state: &mut TurnState) -> Result<()> {
    let Some(params) = params else {
        return Ok(());
    };
    if method == "item/agentMessage/delta" && notification_matches(params, state) {
        if let Some(delta) = params.get("delta").and_then(Value::as_str) {
            state.assistant.push_str(delta);
        }
    } else if method == "thread/tokenUsage/updated" && notification_matches(params, state) {
        state.token_usage = Some(parse_token_usage(params)?);
    } else if method == "item/completed" && notification_matches(params, state) {
        if let Some(transcript) = render_completed_item(params.get("item"))? {
            state.completed_item_transcripts.push(transcript);
        }
    } else if method == "turn/completed" && notification_matches(params, state) {
        state.turn_status = params
            .pointer("/turn/status")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
        state.completed_at = Some(Instant::now());
    }
    Ok(())
}

fn render_completed_item(item: Option<&Value>) -> Result<Option<String>> {
    let Some(item) = item else {
        return Ok(None);
    };
    let item_type = item
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    if matches!(
        item_type,
        "agentMessage" | "plan" | "reasoning" | "userMessage"
    ) {
        return Ok(None);
    }
    let text = serde_json::to_string(item)
        .map_err(|error| Error::Invalid(format!("failed to preserve completed item: {error}")))?;
    Ok(Some(format!("native Codex item completed: {text}")))
}

fn notification_matches(params: &Value, state: &TurnState) -> bool {
    let thread_matches = state
        .thread_id
        .as_deref()
        .is_none_or(|thread_id| params.get("threadId").and_then(Value::as_str) == Some(thread_id));
    let turn_matches = state.turn_id.as_deref().is_none_or(|turn_id| {
        params.get("turnId").and_then(Value::as_str) == Some(turn_id)
            || params.pointer("/turn/id").and_then(Value::as_str) == Some(turn_id)
    });
    thread_matches && turn_matches
}

fn parse_token_usage(params: &Value) -> Result<TokenUsage> {
    let last = params.pointer("/tokenUsage/last").ok_or_else(|| {
        Error::Invalid("token-usage notification omitted tokenUsage.last".to_owned())
    })?;
    Ok(TokenUsage {
        cached_input_tokens: required_u64(last, "cachedInputTokens")?,
        input_tokens: required_u64(last, "inputTokens")?,
        model_context_window: params
            .pointer("/tokenUsage/modelContextWindow")
            .and_then(Value::as_u64),
        output_tokens: required_u64(last, "outputTokens")?,
        reasoning_output_tokens: required_u64(last, "reasoningOutputTokens")?,
        total_tokens: required_u64(last, "totalTokens")?,
    })
}

fn required_u64(value: &Value, key: &str) -> Result<u64> {
    value
        .get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| Error::Invalid(format!("token-usage notification omitted `{key}`")))
}

fn required_string(value: &Value, pointer: &str, label: &str) -> Result<String> {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| Error::Invalid(format!("Codex app-server omitted {label}")))
}

#[cfg(test)]
mod tests {
    use super::{build_model_context, run_protocol, spawn_line_reader, ModelTurnConfig};
    use crate::storage::{Role, Store};
    use serde_json::{json, Value};
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;
    use std::path::Path;
    use std::thread;
    use std::time::Duration;
    use tempfile::tempdir;

    const SUMMARY_PHRASE: &str = "violet-calendar-5481";
    const SUMMARY: &str =
        "Stale raw evidence was compacted; durable code phrase violet-calendar-5481 remains.";
    const PROMPT: &str = "Reply with the durable code phrase from prior context, or ABSENT. Do not run tools or inspect files.";

    #[test]
    fn next_model_payload_and_reported_tokens_shrink_after_compaction() {
        let temp = tempdir().unwrap();
        let db_path = temp.path().join("pcodx.sqlite3");
        let mut store = Store::open(&db_path).unwrap();
        let session = store
            .create_session(Some("model-context"), temp.path())
            .unwrap();
        store
            .record_message(&session, Role::Developer, "developer-retained", None)
            .unwrap();
        store
            .record_message(&session, Role::User, "user-retained", None)
            .unwrap();
        store
            .record_message(&session, Role::Assistant, &bulky_context("alpha"), None)
            .unwrap();
        store
            .record_message(&session, Role::Assistant, &bulky_context("beta"), None)
            .unwrap();
        store
            .record_message(&session, Role::Assistant, "assistant-retained", None)
            .unwrap();
        drop(store);

        let raw = fake_model_turn(&db_path, &session, temp.path());
        let mut store = Store::open(&db_path).unwrap();
        store.compact(&session, "msg3", "msg4", SUMMARY).unwrap();
        drop(store);
        let compacted = fake_model_turn(&db_path, &session, temp.path());

        assert!(raw
            .rendered_model_context
            .contains("PCODX_RAW_CONTEXT_alpha_000"));
        assert!(!compacted
            .rendered_model_context
            .contains("PCODX_RAW_CONTEXT_alpha_000"));
        assert!(compacted.rendered_model_context.contains(SUMMARY_PHRASE));
        assert!(compacted.token_usage.input_tokens < raw.token_usage.input_tokens);
        assert_eq!(raw.assistant, "ABSENT");
        assert_eq!(compacted.assistant, SUMMARY_PHRASE);
        let items: Value = serde_json::from_str(&compacted.rendered_model_context).unwrap();
        let roles: Vec<&str> = items
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["role"].as_str().unwrap())
            .collect();
        assert_eq!(
            &roles[..4],
            &["developer", "user", "assistant", "assistant"]
        );
        assert!(items[0]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("developer-retained\n<aboveturn id=\"msg1\"/>"));
        assert!(items[1]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("user-retained\n<aboveturn id=\"msg2\"/>"));
        assert_eq!(
            items[2]["content"][0]["text"].as_str().unwrap(),
            format!("{SUMMARY}\n<aboveturn id=\"cmp1\"/>")
        );
        assert!(items[3]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("assistant-retained\n<aboveturn id=\"msg5\"/>"));
        assert_eq!(
            compacted.context_strategy,
            "fresh_ephemeral_thread_plus_thread/inject_items"
        );
        assert!(!compacted.active_thread_history_replaced);
        let store = Store::open(&db_path).unwrap();
        assert!(store.messages(&session).unwrap()[2]
            .text
            .contains("PCODX_RAW_CONTEXT_alpha_000"));
        assert_eq!(store.visible_ids(&session).unwrap()[2], "cmp1");
    }

    #[test]
    fn visible_flat_tool_message_requires_compaction_before_model_turn() {
        let temp = tempdir().unwrap();
        let db_path = temp.path().join("pcodx.sqlite3");
        let mut store = Store::open(&db_path).unwrap();
        let session = store
            .create_session(Some("flat-tool"), temp.path())
            .unwrap();
        store
            .record_message(&session, Role::Tool, "unstructured tool output", None)
            .unwrap();

        let error = build_model_context(&store, &session).unwrap_err();

        assert!(error.to_string().contains(
            "visible tool message `msg1` lacks raw Responses API call metadata; compact it"
        ));
    }

    #[test]
    #[ignore = "requires an authenticated Codex app-server and performs two real model turns"]
    fn live_next_model_turn_sees_smaller_compacted_context() {
        let proof_dir = std::env::var_os("PCODX_CONTEXT_PROOF_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| {
                std::path::PathBuf::from("target/pcodx-context-proof")
                    .join(std::process::id().to_string())
            });
        std::fs::create_dir_all(&proof_dir).unwrap();
        let db_path = proof_dir.join("pcodx.sqlite3");
        let mut store = Store::open(&db_path).unwrap();
        let session = store
            .create_session(Some("live-model-context"), &proof_dir)
            .unwrap();
        store
            .record_message(
                &session,
                Role::Assistant,
                &bulky_live_context("alpha"),
                None,
            )
            .unwrap();
        store
            .record_message(&session, Role::Assistant, &bulky_live_context("beta"), None)
            .unwrap();
        drop(store);
        let config = |prompt: &str| ModelTurnConfig {
            codex_bin: std::env::var("PCODX_CODEX_BIN").unwrap_or_else(|_| "codex".to_owned()),
            cwd: std::env::current_dir().unwrap(),
            db_path: db_path.clone(),
            prompt: prompt.to_owned(),
            session_id: session.clone(),
            timeout: Duration::from_secs(180),
        };
        let raw = super::run_model_turn(&config(PROMPT)).unwrap();
        std::fs::write(
            proof_dir.join("raw-model-visible-context.txt"),
            &raw.rendered_model_context,
        )
        .unwrap();
        let mut store = Store::open(&db_path).unwrap();
        store.compact(&session, "msg1", "msg2", SUMMARY).unwrap();
        drop(store);
        let compacted = super::run_model_turn(&config(PROMPT)).unwrap();
        std::fs::write(
            proof_dir.join("compacted-model-visible-context.txt"),
            &compacted.rendered_model_context,
        )
        .unwrap();

        let raw_tokens = raw.token_usage.input_tokens;
        let compacted_tokens = compacted.token_usage.input_tokens;
        let shrink_tokens = raw_tokens.saturating_sub(compacted_tokens);
        let shrink_fraction = shrink_tokens as f64 / raw_tokens as f64;
        assert!(shrink_tokens >= 1_000, "shrink_tokens={shrink_tokens}");
        assert!(shrink_fraction >= 0.4, "shrink_fraction={shrink_fraction}");
        assert!(
            compacted.assistant.to_lowercase().contains(SUMMARY_PHRASE),
            "assistant={:?}",
            compacted.assistant
        );
        assert_eq!(raw.recorded_message_ids.len(), 2, "baseline ran a tool");
        assert_eq!(
            compacted.recorded_message_ids.len(),
            2,
            "follow-up ran a tool"
        );
        assert!(!compacted
            .rendered_model_context
            .contains("PCODX_LIVE_RAW_CONTEXT_alpha_000"));
        let evidence = json!({
            "ok": true,
            "raw_input_tokens": raw_tokens,
            "compacted_input_tokens": compacted_tokens,
            "raw_cached_input_tokens": raw.token_usage.cached_input_tokens,
            "compacted_cached_input_tokens": compacted.token_usage.cached_input_tokens,
            "model_context_window": compacted.token_usage.model_context_window,
            "shrink_tokens": shrink_tokens,
            "shrink_fraction": shrink_fraction,
            "raw_context_chars": raw.injected_context_chars,
            "compacted_context_chars": compacted.injected_context_chars,
            "raw_thread_id": raw.upstream_thread_id,
            "compacted_thread_id": compacted.upstream_thread_id,
            "follow_up_assistant": compacted.assistant,
            "context_strategy": compacted.context_strategy,
            "active_thread_history_replaced": compacted.active_thread_history_replaced,
            "kv_cache_status": compacted.kv_cache_status,
            "proof_dir": proof_dir,
        });
        std::fs::write(
            proof_dir.join("result.json"),
            serde_json::to_string_pretty(&evidence).unwrap(),
        )
        .unwrap();
        println!("{}", serde_json::to_string_pretty(&evidence).unwrap());
    }

    fn fake_model_turn(db_path: &Path, session: &str, cwd: &Path) -> super::ModelTurnResult {
        let (controller, server) = UnixStream::pair().unwrap();
        let controller_reader = controller.try_clone().unwrap();
        let lines = spawn_line_reader(controller_reader);
        let fake = thread::spawn(move || run_fake_app_server(server));
        let mut store = Store::open(db_path).unwrap();
        let context = build_model_context(&store, session).unwrap();
        let config = ModelTurnConfig {
            codex_bin: "unused".to_owned(),
            cwd: cwd.to_path_buf(),
            db_path: db_path.to_path_buf(),
            prompt: PROMPT.to_owned(),
            session_id: session.to_owned(),
            timeout: Duration::from_secs(5),
        };
        let mut controller = controller;
        let result = run_protocol(&mut controller, &lines, &mut store, &config, context).unwrap();
        drop(controller);
        fake.join().unwrap();
        result
    }

    fn run_fake_app_server(stream: UnixStream) {
        let mut write = stream.try_clone().unwrap();
        let mut injected_context = String::new();
        for line in BufReader::new(stream).lines() {
            let request: Value = serde_json::from_str(&line.unwrap()).unwrap();
            let Some(method) = request.get("method").and_then(Value::as_str) else {
                continue;
            };
            let Some(id) = request.get("id").cloned() else {
                continue;
            };
            match method {
                "initialize" => reply(&mut write, id, json!({})),
                "thread/start" => {
                    reply(&mut write, id, json!({ "thread": { "id": "fake-thread" } }))
                }
                "thread/inject_items" => {
                    injected_context = serde_json::to_string(&request["params"]["items"]).unwrap();
                    reply(&mut write, id, json!({}));
                }
                "turn/start" => {
                    reply(
                        &mut write,
                        id,
                        json!({ "turn": { "id": "fake-turn", "status": "inProgress" } }),
                    );
                    let assistant = if injected_context.contains(SUMMARY_PHRASE) {
                        SUMMARY_PHRASE
                    } else {
                        "ABSENT"
                    };
                    notify(
                        &mut write,
                        "item/agentMessage/delta",
                        json!({
                            "threadId": "fake-thread",
                            "turnId": "fake-turn",
                            "itemId": "fake-item",
                            "delta": assistant,
                        }),
                    );
                    let input_tokens = u64::try_from(injected_context.len() / 4 + 100).unwrap();
                    notify(
                        &mut write,
                        "thread/tokenUsage/updated",
                        json!({
                            "threadId": "fake-thread",
                            "turnId": "fake-turn",
                            "tokenUsage": {
                                "last": token_usage(input_tokens),
                                "total": token_usage(input_tokens),
                                "modelContextWindow": 200_000,
                            },
                        }),
                    );
                    notify(
                        &mut write,
                        "turn/completed",
                        json!({
                            "threadId": "fake-thread",
                            "turn": { "id": "fake-turn", "status": "completed" },
                        }),
                    );
                    return;
                }
                _ => panic!("unexpected fake app-server request {method}"),
            }
        }
    }

    fn reply(write: &mut UnixStream, id: Value, result: Value) {
        writeln!(write, "{}", json!({ "id": id, "result": result })).unwrap();
        write.flush().unwrap();
    }

    fn notify(write: &mut UnixStream, method: &str, params: Value) {
        writeln!(write, "{}", json!({ "method": method, "params": params })).unwrap();
        write.flush().unwrap();
    }

    fn token_usage(input_tokens: u64) -> Value {
        json!({
            "cachedInputTokens": 0,
            "inputTokens": input_tokens,
            "outputTokens": 1,
            "reasoningOutputTokens": 0,
            "totalTokens": input_tokens + 1,
        })
    }

    fn bulky_context(label: &str) -> String {
        (0..80)
            .map(|idx| {
                format!(
                    "PCODX_RAW_CONTEXT_{label}_{idx:03} stale redundant transcript that must disappear"
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn bulky_live_context(label: &str) -> String {
        (0..180)
            .map(|idx| {
                format!(
                    "PCODX_LIVE_RAW_CONTEXT_{label}_{idx:03} | stale verifier transcript with redundant command output | requestTimeoutMs=30000 upstreamDeadlineMs=9000 | this line must disappear after compaction"
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}
