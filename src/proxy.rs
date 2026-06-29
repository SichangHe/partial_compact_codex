use crate::prompts;
use crate::storage::{Error, Result, Store};
use crate::tool_endpoint::{self, ToolConfig};
use serde_json::{json, Value};
use std::fs;
use std::io;
use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::os::unix::fs::FileTypeExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};
use tungstenite::error::Error as WsError;
use tungstenite::handshake::{HandshakeError, HandshakeRole};
use tungstenite::protocol::WebSocket;
use tungstenite::Message;

pub struct ProxyConfig {
    pub listen: String,
    pub upstream: String,
    pub codex_bin: String,
    pub launch_upstream: bool,
    pub tools: Option<ProxyToolConfig>,
}

#[derive(Clone)]
pub struct ProxyToolConfig {
    pub db_path: PathBuf,
    pub session_id: String,
}

pub fn serve(config: ProxyConfig) -> Result<()> {
    let listen = Endpoint::parse(&config.listen)?;
    let upstream = Endpoint::parse(&config.upstream)?;
    prepare_listen_endpoint(&listen)?;
    prepare_upstream_endpoint(&upstream, config.launch_upstream)?;
    let mut upstream_process = if config.launch_upstream {
        Some(Upstream::start(&config.codex_bin, &config.upstream)?)
    } else {
        None
    };
    if let Some(process) = upstream_process.as_mut() {
        process.wait_until_ready(&upstream)?;
    }
    println!("pcodx_proxy_listen={}", config.listen);
    println!("upstream_codex_app_server={}", config.upstream);
    println!("codex_frontend=codex --remote {}", config.listen);
    println!("pcodx_live_context_mutation=none");
    println!("native_codex_mutations=relayed");
    println!("pcodx_tool_boundary=websocket_text_json_rpc_thread_start_and_item_tool_call");
    println!("pcodx_thread_mapping=single_pcodx_session_per_serve_process");
    match (listen, upstream) {
        (Endpoint::Unix(listen), Endpoint::Unix(upstream)) => serve_unix(&listen, &upstream),
        (Endpoint::Ws(listen), Endpoint::Ws(upstream)) => {
            serve_ws(&listen, &upstream, config.tools)
        }
        _ => Err(Error::Invalid(
            "listen and upstream must use the same transport".to_owned(),
        )),
    }
}

fn serve_unix(listen: &Path, upstream: &Path) -> Result<()> {
    let listener = UnixListener::bind(listen)?;
    for client in listener.incoming() {
        relay_unix(client?, UnixStream::connect(upstream)?)?;
    }
    Ok(())
}

fn serve_ws(listen: &str, upstream: &str, tools: Option<ProxyToolConfig>) -> Result<()> {
    let listener = TcpListener::bind(listen)?;
    for client in listener.incoming() {
        relay_ws(client?, upstream, tools.clone())?;
    }
    Ok(())
}

fn relay_unix(client: UnixStream, upstream: UnixStream) -> Result<()> {
    let client_read = client.try_clone()?;
    let upstream_read = upstream.try_clone()?;
    let up = thread::spawn(move || copy_raw_and_shutdown(client_read, upstream));
    let down = thread::spawn(move || copy_raw_and_shutdown(upstream_read, client));
    join_copy(up)?;
    join_copy(down)?;
    Ok(())
}

fn copy_raw_and_shutdown<R, W>(mut read: R, mut write: W) -> io::Result<u64>
where
    R: Read,
    W: Write + ShutdownWrite,
{
    let n_bytes = io::copy(&mut read, &mut write)?;
    write.shutdown_write()?;
    Ok(n_bytes)
}

fn relay_ws(client: TcpStream, upstream: &str, tools: Option<ProxyToolConfig>) -> Result<()> {
    let mut client = tungstenite::accept(client).map_err(handshake_error)?;
    let upstream_url = format!("ws://{upstream}");
    let upstream_stream = TcpStream::connect(upstream)?;
    let (mut upstream, _) =
        tungstenite::client(upstream_url.as_str(), upstream_stream).map_err(handshake_error)?;
    client.get_mut().set_nonblocking(true)?;
    upstream.get_mut().set_nonblocking(true)?;
    let mut capture = WsFixtureCapture::from_env()?;
    loop {
        let mut active = false;
        match relay_ws_message(
            &mut client,
            &mut upstream,
            tools.as_ref(),
            ProxyDirection::ClientToUpstream,
            &mut capture,
        ) {
            Ok(WsRelay::Active) => active = true,
            Ok(WsRelay::Idle) => {}
            Ok(WsRelay::Closed) => return Ok(()),
            Err(error) => return Err(error),
        }
        match relay_ws_message(
            &mut upstream,
            &mut client,
            tools.as_ref(),
            ProxyDirection::UpstreamToClient,
            &mut capture,
        ) {
            Ok(WsRelay::Active) => active = true,
            Ok(WsRelay::Idle) => {}
            Ok(WsRelay::Closed) => return Ok(()),
            Err(error) => return Err(error),
        }
        if !active {
            thread::sleep(Duration::from_millis(5));
        }
    }
}

enum WsRelay {
    Active,
    Idle,
    Closed,
}

fn relay_ws_message<R, W>(
    read: &mut WebSocket<R>,
    write: &mut WebSocket<W>,
    tools: Option<&ProxyToolConfig>,
    direction: ProxyDirection,
    capture: &mut Option<WsFixtureCapture>,
) -> Result<WsRelay>
where
    R: Read + Write,
    W: Read + Write,
{
    let message = match read.read() {
        Ok(message) => message,
        Err(WsError::Io(error)) if error.kind() == io::ErrorKind::WouldBlock => {
            return Ok(WsRelay::Idle)
        }
        Err(WsError::ConnectionClosed) => return Ok(WsRelay::Closed),
        Err(error) => return Err(ws_error(error)),
    };
    if let (Message::Text(text), Some(capture)) = (&message, capture.as_mut()) {
        capture.record(direction, text.as_str())?;
    }
    let output = transform_ws_message(message, tools, direction);
    match output {
        WsMessageOutput::Forward(message) => send_ws_message(write, message)?,
        WsMessageOutput::RespondBack(message) => send_ws_message(read, message)?,
        WsMessageOutput::Close(frame) => {
            let _ = write.close(frame.clone());
            let _ = read.close(frame);
            return Ok(WsRelay::Closed);
        }
    }
    Ok(WsRelay::Active)
}

fn send_ws_message<W>(write: &mut WebSocket<W>, message: Message) -> Result<()>
where
    W: Read + Write,
{
    match write.write(message) {
        Ok(()) => {}
        Err(WsError::Io(error)) if error.kind() == io::ErrorKind::WouldBlock => {}
        Err(error) => return Err(ws_error(error)),
    }
    loop {
        match write.flush() {
            Ok(()) => return Ok(()),
            Err(WsError::Io(error)) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(5));
            }
            Err(error) => return Err(ws_error(error)),
        }
    }
}

enum WsMessageOutput {
    Forward(Message),
    RespondBack(Message),
    Close(Option<tungstenite::protocol::CloseFrame>),
}

fn transform_ws_message(
    message: Message,
    tools: Option<&ProxyToolConfig>,
    direction: ProxyDirection,
) -> WsMessageOutput {
    let Message::Text(text) = message else {
        return match message {
            Message::Close(frame) => WsMessageOutput::Close(frame),
            message => WsMessageOutput::Forward(message),
        };
    };
    match transform_proxy_chunk(text.as_str().as_bytes(), tools, direction) {
        ProxyChunk::Forward(output) => {
            let text = String::from_utf8(output).unwrap_or_else(|_| text.to_string());
            WsMessageOutput::Forward(Message::text(text))
        }
        ProxyChunk::RespondUpstream(output) => {
            let text = String::from_utf8(output).unwrap_or_else(|_| text.to_string());
            WsMessageOutput::RespondBack(Message::text(text))
        }
    }
}

fn ws_error(error: WsError) -> Error {
    Error::Invalid(format!("websocket relay failed: {error}"))
}

fn handshake_error<S: HandshakeRole>(error: HandshakeError<S>) -> Error {
    Error::Invalid(format!("websocket handshake failed: {error}"))
}

trait ShutdownWrite {
    fn shutdown_write(&mut self) -> io::Result<()>;
}

impl ShutdownWrite for UnixStream {
    fn shutdown_write(&mut self) -> io::Result<()> {
        self.shutdown(std::net::Shutdown::Write)
    }
}

impl ShutdownWrite for TcpStream {
    fn shutdown_write(&mut self) -> io::Result<()> {
        self.shutdown(Shutdown::Write)
    }
}

#[derive(Clone, Copy)]
enum ProxyDirection {
    ClientToUpstream,
    UpstreamToClient,
}

impl ProxyDirection {
    fn fixture_prefix(self) -> &'static str {
        match self {
            Self::ClientToUpstream => "client_to_upstream",
            Self::UpstreamToClient => "upstream_to_client",
        }
    }
}

struct WsFixtureCapture {
    dir: PathBuf,
    seq: usize,
    lifecycle_request_ids: Vec<(Value, String)>,
}

impl WsFixtureCapture {
    fn from_env() -> Result<Option<Self>> {
        let Some(dir) = std::env::var_os("PCODX_WS_FIXTURE_DIR").map(PathBuf::from) else {
            return Ok(None);
        };
        fs::create_dir_all(&dir)?;
        let seq = existing_fixture_seq(&dir)?;
        Ok(Some(Self {
            dir,
            seq,
            lifecycle_request_ids: Vec::new(),
        }))
    }

    fn record(&mut self, direction: ProxyDirection, text: &str) -> Result<()> {
        self.seq += 1;
        let value = serde_json::from_str::<Value>(text).ok();
        if let Some(method) = lifecycle_request_method(value.as_ref()) {
            if let Some(id) = value.as_ref().and_then(|value| value.get("id")).cloned() {
                self.lifecycle_request_ids
                    .push((id, sanitize_fixture_label(method)));
            }
        }
        let label = fixture_label(direction, value.as_ref(), &self.lifecycle_request_ids);
        let path = self.dir.join(format!("{:04}_{label}.json", self.seq));
        fs::write(path, pretty_or_raw_json(text))?;
        Ok(())
    }
}

fn fixture_label(
    direction: ProxyDirection,
    value: Option<&Value>,
    lifecycle_request_ids: &[(Value, String)],
) -> String {
    let prefix = direction.fixture_prefix();
    let Some(value) = value else {
        return format!("{prefix}_text");
    };
    if let Some(method) = lifecycle_response_method(value, lifecycle_request_ids) {
        return format!("{prefix}_{method}_response");
    }
    let Some(method) = value.get("method").and_then(Value::as_str) else {
        return format!("{prefix}_json_rpc");
    };
    format!("{prefix}_{}", sanitize_fixture_label(method))
}

fn lifecycle_request_method(value: Option<&Value>) -> Option<&str> {
    let method = value
        .and_then(|value| value.get("method"))
        .and_then(Value::as_str)?;
    matches!(method, "thread/start" | "thread/resume" | "thread/fork").then_some(method)
}

fn lifecycle_response_method<'a>(
    value: &Value,
    lifecycle_request_ids: &'a [(Value, String)],
) -> Option<&'a str> {
    let id = value.get("id")?;
    value.get("result")?;
    lifecycle_request_ids
        .iter()
        .find(|(candidate, _)| candidate == id)
        .map(|(_, method)| method.as_str())
}

fn existing_fixture_seq(dir: &Path) -> Result<usize> {
    let mut max_seq = 0;
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let Some(name) = entry.file_name().to_str().map(ToOwned::to_owned) else {
            continue;
        };
        let Some((prefix, _)) = name.split_once('_') else {
            continue;
        };
        let Ok(seq) = prefix.parse::<usize>() else {
            continue;
        };
        max_seq = max_seq.max(seq);
    }
    Ok(max_seq)
}

fn sanitize_fixture_label(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

fn pretty_or_raw_json(text: &str) -> String {
    serde_json::from_str::<Value>(text)
        .and_then(|value| serde_json::to_string_pretty(&value))
        .unwrap_or_else(|_| text.to_owned())
}

enum ProxyChunk {
    Forward(Vec<u8>),
    RespondUpstream(Vec<u8>),
}

fn join_copy(handle: thread::JoinHandle<io::Result<u64>>) -> Result<()> {
    handle
        .join()
        .map_err(|_| Error::Invalid("proxy copy thread panicked".to_owned()))??;
    Ok(())
}

fn transform_proxy_chunk(
    input: &[u8],
    tools: Option<&ProxyToolConfig>,
    direction: ProxyDirection,
) -> ProxyChunk {
    let Some(tools) = tools else {
        return ProxyChunk::Forward(input.to_vec());
    };
    let Ok(text) = std::str::from_utf8(input) else {
        return ProxyChunk::Forward(input.to_vec());
    };
    let transformed = match direction {
        ProxyDirection::ClientToUpstream => {
            return ProxyChunk::Forward(
                register_pcodx_tools(text).map_or_else(|| input.to_vec(), |text| text.into_bytes()),
            )
        }
        ProxyDirection::UpstreamToClient => handle_pcodx_tool_call(text, tools),
    };
    transformed.map_or_else(
        || ProxyChunk::Forward(input.to_vec()),
        |text| ProxyChunk::RespondUpstream(text.into_bytes()),
    )
}

fn register_pcodx_tools(text: &str) -> Option<String> {
    let mut value = parse_json_rpc_object(text)?;
    let method = value.get("method")?.as_str()?;
    if method != "thread/start" {
        return None;
    }
    let params = value
        .as_object_mut()?
        .entry("params")
        .or_insert_with(|| json!({}));
    let params = ensure_object(params)?;
    merge_developer_instructions(params);
    merge_dynamic_tools(params);
    serde_json::to_string(&value).ok()
}

fn handle_pcodx_tool_call(text: &str, cfg: &ProxyToolConfig) -> Option<String> {
    let value = parse_json_rpc_object(text)?;
    if value.get("method")?.as_str()? != "item/tool/call" {
        return None;
    }
    let id = value.get("id")?.clone();
    let params = value.get("params")?.as_object()?;
    let tool = params.get("tool")?.as_str()?;
    if !is_pcodx_tool(tool) {
        return None;
    }
    let result = execute_pcodx_tool(tool, params.get("arguments"), cfg);
    Some(json!({ "id": id, "result": result }).to_string())
}

fn execute_pcodx_tool(tool: &str, arguments: Option<&Value>, cfg: &ProxyToolConfig) -> Value {
    let text = match tool {
        "partial_compact" => {
            let args_json = arguments
                .map(Value::to_string)
                .unwrap_or_else(|| "{}".to_owned());
            match Store::open(&cfg.db_path) {
                Ok(mut store) => tool_endpoint::partial_compact_json(
                    &mut store,
                    &cfg.session_id,
                    &args_json,
                    ToolConfig::default(),
                ),
                Err(error) => {
                    json!({ "error": format!("session {}: {error}", cfg.session_id) }).to_string()
                }
            }
        }
        "partial_compact_current_session_message_ids" => match Store::open(&cfg.db_path) {
            Ok(store) => tool_endpoint::current_session_message_ids_tool(&store, &cfg.session_id),
            Err(error) => {
                json!({ "error": format!("session {}: {error}", cfg.session_id) }).to_string()
            }
        },
        "partial_compact_instructions" => prompts::get("partial-compact-instruction.md")
            .expect("partial-compact-instruction.md is embedded")
            .trim()
            .to_owned(),
        _ => json!({ "error": format!("unknown PCODX dynamic tool {tool}") }).to_string(),
    };
    let success = serde_json::from_str::<Value>(&text)
        .ok()
        .and_then(|value| value.get("error").cloned())
        .is_none();
    json!({
        "contentItems": [{ "type": "inputText", "text": text }],
        "success": success,
    })
}

fn parse_json_rpc_object(text: &str) -> Option<Value> {
    let value = serde_json::from_str::<Value>(text.trim()).ok()?;
    value.is_object().then_some(value)
}

fn ensure_object(value: &mut Value) -> Option<&mut serde_json::Map<String, Value>> {
    if !value.is_object() {
        *value = json!({});
    }
    value.as_object_mut()
}

fn merge_developer_instructions(params: &mut serde_json::Map<String, Value>) {
    let pcodx = prompts::partial_compact_developer_instructions();
    let existing = params
        .get("developerInstructions")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if existing.contains(&pcodx) {
        return;
    }
    let merged = if existing.is_empty() {
        pcodx
    } else {
        format!("{existing}\n\n{pcodx}")
    };
    params.insert("developerInstructions".to_owned(), json!(merged));
}

fn merge_dynamic_tools(params: &mut serde_json::Map<String, Value>) {
    let mut tools = params
        .get("dynamicTools")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut names: Vec<String> = tools
        .iter()
        .filter_map(|tool| tool.get("name")?.as_str().map(ToOwned::to_owned))
        .collect();
    for tool in pcodx_dynamic_tools() {
        let Some(name) = tool.get("name").and_then(Value::as_str) else {
            continue;
        };
        if names.iter().any(|existing| existing == name) {
            continue;
        }
        names.push(name.to_owned());
        tools.push(tool);
    }
    params.insert("dynamicTools".to_owned(), Value::Array(tools));
}

fn pcodx_dynamic_tools() -> Vec<Value> {
    vec![
        json!({
            "type": "function",
            "name": "partial_compact_instructions",
            "description": prompts::get("partial-compact-instruction-tool-description.md")
                .expect("partial-compact-instruction-tool-description.md is embedded")
                .trim(),
            "inputSchema": {
                "type": "object",
                "properties": {},
                "additionalProperties": false,
            },
        }),
        json!({
            "type": "function",
            "name": "partial_compact_current_session_message_ids",
            "description": prompts::get("current-session-message-ids-tool-description.md")
                .expect("current-session-message-ids-tool-description.md is embedded")
                .trim(),
            "inputSchema": {
                "type": "object",
                "properties": {},
                "additionalProperties": false,
            },
        }),
        json!({
            "type": "function",
            "name": "partial_compact",
            "description": prompts::partial_compact_tool_description(),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "ranges": {
                        "type": "array",
                        "description": prompts::get("partial-compact-arg-ranges.md")
                            .expect("partial-compact-arg-ranges.md is embedded")
                            .trim(),
                        "minItems": 1,
                        "items": {
                            "type": "object",
                            "properties": {
                                "from_message_id": {
                                    "type": "string",
                                    "description": prompts::get("partial-compact-range-from-message-id.md")
                                        .expect("partial-compact-range-from-message-id.md is embedded")
                                        .trim(),
                                },
                                "to_message_id": {
                                    "type": "string",
                                    "description": prompts::get("partial-compact-range-to-message-id.md")
                                        .expect("partial-compact-range-to-message-id.md is embedded")
                                        .trim(),
                                },
                                "summary": {
                                    "type": "string",
                                    "description": prompts::get("partial-compact-range-summary.md")
                                        .expect("partial-compact-range-summary.md is embedded")
                                        .trim(),
                                },
                            },
                            "required": ["from_message_id", "to_message_id", "summary"],
                            "additionalProperties": false,
                        },
                    },
                },
                "required": ["ranges"],
                "additionalProperties": false,
            },
        }),
    ]
}

fn is_pcodx_tool(tool: &str) -> bool {
    matches!(
        tool,
        "partial_compact"
            | "partial_compact_current_session_message_ids"
            | "partial_compact_instructions"
    )
}

fn ensure_parent(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    Ok(())
}

fn prepare_listen_endpoint(endpoint: &Endpoint) -> Result<()> {
    match endpoint {
        Endpoint::Unix(path) => {
            ensure_parent(path)?;
            remove_dead_socket(path)
        }
        Endpoint::Ws(_) => Ok(()),
    }
}

fn prepare_upstream_endpoint(endpoint: &Endpoint, launch_upstream: bool) -> Result<()> {
    if !launch_upstream {
        return Ok(());
    }
    match endpoint {
        Endpoint::Unix(path) => {
            ensure_parent(path)?;
            remove_dead_socket(path)
        }
        Endpoint::Ws(_) => Ok(()),
    }
}

fn remove_dead_socket(path: &Path) -> Result<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    if !metadata.file_type().is_socket() {
        return Err(Error::Invalid(format!(
            "refusing to remove non-socket path {}",
            path.display()
        )));
    }
    if UnixStream::connect(path).is_ok() {
        return Err(Error::Invalid(format!(
            "refusing to remove active socket {}",
            path.display()
        )));
    }
    fs::remove_file(path)?;
    Ok(())
}

enum Endpoint {
    Unix(PathBuf),
    Ws(String),
}

impl Endpoint {
    fn parse(value: &str) -> Result<Self> {
        if let Some(path) = value.strip_prefix("unix://") {
            if path.is_empty() {
                return Err(Error::Invalid("unix endpoint needs a path".to_owned()));
            }
            return Ok(Self::Unix(PathBuf::from(path)));
        }
        if let Some(addr) = value.strip_prefix("ws://") {
            if addr.is_empty() || addr.contains('/') {
                return Err(Error::Invalid(
                    "ws endpoint must be ws://HOST:PORT".to_owned(),
                ));
            }
            return Ok(Self::Ws(addr.to_owned()));
        }
        Err(Error::Invalid(
            "endpoint must start with ws:// or unix://".to_owned(),
        ))
    }
}

struct Upstream {
    child: Child,
    started_at: Instant,
}

impl Upstream {
    fn start(codex_bin: &str, upstream: &str) -> Result<Self> {
        let child = Command::new(codex_bin)
            .args(["app-server", "--listen", upstream])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()?;
        Ok(Self {
            child,
            started_at: Instant::now(),
        })
    }

    fn wait_until_ready(&mut self, upstream: &Endpoint) -> Result<()> {
        while self.started_at.elapsed() < Duration::from_secs(10) {
            if endpoint_is_ready(upstream) {
                return Ok(());
            }
            if let Some(status) = self.child.try_wait()? {
                return Err(Error::Invalid(format!(
                    "Codex app-server exited before accepting {upstream}: {status}"
                )));
            }
            thread::sleep(Duration::from_millis(50));
        }
        Err(Error::Invalid(format!(
            "timed out waiting for Codex app-server endpoint {upstream}"
        )))
    }
}

impl Drop for Upstream {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn endpoint_is_ready(endpoint: &Endpoint) -> bool {
    match endpoint {
        Endpoint::Unix(path) => UnixStream::connect(path).is_ok(),
        Endpoint::Ws(addr) => TcpStream::connect(addr).is_ok(),
    }
}

impl std::fmt::Display for Endpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unix(path) => write!(f, "unix://{}", path.display()),
            Self::Ws(addr) => write!(f, "ws://{addr}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        existing_fixture_seq, fixture_label, handle_pcodx_tool_call, register_pcodx_tools,
        transform_proxy_chunk, transform_ws_message, ProxyChunk, ProxyDirection, ProxyToolConfig,
        WsMessageOutput,
    };
    use crate::storage::{Role, Store};
    use serde_json::{json, Value};
    use tempfile::tempdir;
    use tungstenite::Message;

    #[test]
    fn thread_start_registers_pcodx_dynamic_tools() {
        let output = register_pcodx_tools(
            r#"{"id":1,"method":"thread/start","params":{"dynamicTools":[{"name":"native_tool"}],"developerInstructions":"keep this"}}"#,
        )
        .unwrap();
        let value: Value = serde_json::from_str(&output).unwrap();
        let tools = value["params"]["dynamicTools"].as_array().unwrap();
        let names: Vec<_> = tools
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"native_tool"));
        assert!(names.contains(&"partial_compact"));
        assert!(names.contains(&"partial_compact_current_session_message_ids"));
        assert!(tools
            .iter()
            .filter(|tool| tool["name"]
                .as_str()
                .is_some_and(|name| name.starts_with("partial_compact")))
            .all(|tool| tool["type"] == "function"));
        assert!(value["params"]["developerInstructions"]
            .as_str()
            .unwrap()
            .contains("PCODX partial compaction is available"));
    }

    #[test]
    fn thread_resume_and_fork_do_not_register_dynamic_tools() {
        for method in ["thread/resume", "thread/fork"] {
            assert!(register_pcodx_tools(
                &json!({
                    "id": 1,
                    "method": method,
                    "params": {
                        "dynamicTools": [],
                        "developerInstructions": ""
                    }
                })
                .to_string()
            )
            .is_none());
        }
    }

    #[test]
    fn item_tool_call_routes_partial_compact_to_rust_endpoint() {
        let temp = tempdir().unwrap();
        let db_path = temp.path().join("pcodx.sqlite3");
        let mut store = Store::open(&db_path).unwrap();
        let session = store
            .create_session(Some("ses-proxy"), temp.path())
            .unwrap();
        store
            .record_message(&session, Role::Assistant, "old setup", None)
            .unwrap();
        store
            .record_message(&session, Role::Assistant, "keep this", None)
            .unwrap();
        drop(store);
        let output = handle_pcodx_tool_call(
            &json!({
                "id": 7,
                "method": "item/tool/call",
                "params": {
                    "tool": "partial_compact",
                    "arguments": {
                        "ranges": [{
                            "from_message_id": "msg1",
                            "to_message_id": "msg1",
                            "summary": "setup"
                        }]
                    }
                }
            })
            .to_string(),
            &ProxyToolConfig {
                db_path: db_path.clone(),
                session_id: session.clone(),
            },
        )
        .unwrap();
        let value: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(value["id"], 7);
        assert_eq!(value["result"]["success"], true);
        let text = value["result"]["contentItems"][0]["text"].as_str().unwrap();
        let receipt: Value = serde_json::from_str(text).unwrap();
        assert_eq!(receipt["n_ranges_compacted"], 1);
        let store = Store::open(&db_path).unwrap();
        assert_eq!(
            store.visible_ids(&session).unwrap(),
            vec!["cmp1".to_owned(), "msg2".to_owned()]
        );
    }

    #[test]
    fn item_tool_call_response_is_routed_back_upstream() {
        let temp = tempdir().unwrap();
        let db_path = temp.path().join("pcodx.sqlite3");
        let mut store = Store::open(&db_path).unwrap();
        let session = store
            .create_session(Some("ses-route"), temp.path())
            .unwrap();
        store
            .record_message(&session, Role::Assistant, "old setup", None)
            .unwrap();
        drop(store);
        let chunk = transform_proxy_chunk(
            br#"{"id":7,"method":"item/tool/call","params":{"tool":"partial_compact_current_session_message_ids","arguments":{}}}"#,
            Some(&ProxyToolConfig {
                db_path,
                session_id: session,
            }),
            ProxyDirection::UpstreamToClient,
        );
        match chunk {
            ProxyChunk::RespondUpstream(output) => {
                let value: Value = serde_json::from_slice(&output).unwrap();
                assert_eq!(value["id"], 7);
                assert_eq!(value["result"]["success"], true);
            }
            ProxyChunk::Forward(_) => panic!("PCODX tool call must not be forwarded to client"),
        }
    }

    #[test]
    fn websocket_text_thread_start_registers_pcodx_tools() {
        let output = transform_ws_message(
            Message::text(
                r#"{"id":1,"method":"thread/start","params":{"dynamicTools":[],"developerInstructions":""}}"#,
            ),
            Some(&ProxyToolConfig {
                db_path: "unused.sqlite3".into(),
                session_id: "ses-ws".to_owned(),
            }),
            ProxyDirection::ClientToUpstream,
        );
        match output {
            WsMessageOutput::Forward(Message::Text(text)) => {
                let value: Value = serde_json::from_str(text.as_str()).unwrap();
                let tools = value["params"]["dynamicTools"].as_array().unwrap();
                assert!(tools.iter().any(|tool| tool["name"] == "partial_compact"));
                assert!(tools
                    .iter()
                    .any(|tool| tool["name"] == "partial_compact" && tool["type"] == "function"));
                assert!(value["params"]["developerInstructions"]
                    .as_str()
                    .unwrap()
                    .contains("PCODX partial compaction is available"));
            }
            _ => panic!("websocket thread/start must remain a forwarded text frame"),
        }
    }

    #[test]
    fn websocket_text_item_tool_call_responds_back_to_upstream() {
        let temp = tempdir().unwrap();
        let db_path = temp.path().join("pcodx.sqlite3");
        let mut store = Store::open(&db_path).unwrap();
        let session = store
            .create_session(Some("ses-ws-tool"), temp.path())
            .unwrap();
        store
            .record_message(&session, Role::Assistant, "old setup", None)
            .unwrap();
        drop(store);
        let output = transform_ws_message(
            Message::text(
                r#"{"id":7,"method":"item/tool/call","params":{"tool":"partial_compact_current_session_message_ids","arguments":{}}}"#,
            ),
            Some(&ProxyToolConfig {
                db_path,
                session_id: session,
            }),
            ProxyDirection::UpstreamToClient,
        );
        match output {
            WsMessageOutput::RespondBack(Message::Text(text)) => {
                let value: Value = serde_json::from_str(text.as_str()).unwrap();
                assert_eq!(value["id"], 7);
                assert_eq!(value["result"]["success"], true);
            }
            _ => panic!("PCODX websocket tool call must respond to upstream"),
        }
    }

    #[test]
    fn item_tool_call_routes_current_session_ids_to_rust_endpoint() {
        let temp = tempdir().unwrap();
        let db_path = temp.path().join("pcodx.sqlite3");
        let mut store = Store::open(&db_path).unwrap();
        let session = store
            .create_session(Some("ses-proxy"), temp.path())
            .unwrap();
        store
            .record_message(&session, Role::Assistant, "old setup", None)
            .unwrap();
        drop(store);
        let output = handle_pcodx_tool_call(
            r#"{"id":"a","method":"item/tool/call","params":{"tool":"partial_compact_current_session_message_ids","arguments":{}}}"#,
            &ProxyToolConfig {
                db_path,
                session_id: session,
            },
        )
        .unwrap();
        let value: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(value["id"], "a");
        assert_eq!(value["result"]["success"], true);
        assert!(value["result"]["contentItems"][0]["text"]
            .as_str()
            .unwrap()
            .contains("- msg1"));
    }

    #[test]
    fn fixture_label_tracks_thread_start_response_id() {
        let request: Value = serde_json::from_str(
            r#"{"id":"start-1","method":"thread/start","params":{"dynamicTools":[]}}"#,
        )
        .unwrap();
        let ids = vec![(request["id"].clone(), "thread_start".to_owned())];
        let response: Value =
            serde_json::from_str(r#"{"id":"start-1","result":{"threadId":"abc"}}"#).unwrap();
        assert_eq!(
            fixture_label(ProxyDirection::UpstreamToClient, Some(&response), &ids),
            "upstream_to_client_thread_start_response"
        );
    }

    #[test]
    fn fixture_label_sanitizes_json_rpc_method_names() {
        let value: Value = serde_json::from_str(r#"{"method":"item/tool/call"}"#).unwrap();
        assert_eq!(
            fixture_label(ProxyDirection::UpstreamToClient, Some(&value), &[]),
            "upstream_to_client_item_tool_call"
        );
    }

    #[test]
    fn fixture_label_tracks_resume_and_fork_response_ids() {
        let ids = vec![
            (json!(11), "thread_resume".to_owned()),
            (json!("fork-a"), "thread_fork".to_owned()),
        ];
        let resume: Value =
            serde_json::from_str(r#"{"id":11,"result":{"thread":{"id":"t"}}}"#).unwrap();
        let fork: Value =
            serde_json::from_str(r#"{"id":"fork-a","result":{"thread":{"id":"f"}}}"#).unwrap();
        assert_eq!(
            fixture_label(ProxyDirection::UpstreamToClient, Some(&resume), &ids),
            "upstream_to_client_thread_resume_response"
        );
        assert_eq!(
            fixture_label(ProxyDirection::UpstreamToClient, Some(&fork), &ids),
            "upstream_to_client_thread_fork_response"
        );
    }

    #[test]
    fn fixture_capture_appends_after_existing_numbered_files() {
        let temp = tempdir().unwrap();
        std::fs::write(
            temp.path()
                .join("0042_upstream_to_client_thread_start_response.json"),
            "{}",
        )
        .unwrap();
        std::fs::write(temp.path().join("note.txt"), "ignore").unwrap();
        assert_eq!(existing_fixture_seq(temp.path()).unwrap(), 42);
    }
}
