# pcodx

Codex, but with partial compaction.

The product shape is Codex on both sides: Codex TUI is the frontend, Codex app-server/model path is the backend, and `pcodx` sits between them to do partial compaction. The wrapper should preserve KV-cache-compatible context as much as possible.

The wrapper changes context only in two cases:

- append a minimal turn id after a completed turn
- replace a compacted range with the summary supplied by the agent

The current Rust prototype implements the durable context/rendering core and a transparent real Codex app-server proxy. The proxy uses real Codex TUI on the frontend and real `codex app-server` on the backend. The Rust rewrite is now driven by the TypeScript Codex wrapper proof in `docs/codex-wrapper-poc.md`; the older OpenCode transfer notes are background.

## cli

```sh
pcodx init --session work
pcodx turn --session work --text "first human prompt"
pcodx turn --session work --text-file prompt.md
pcodx record --session work --role assistant --text "large stale discovery"
pcodx compact --session work --from msg2 --to msg2 --summary "discovery was stale; durable fact ..."
pcodx compact-many --session work --range "msg1..msg1=old setup" --range "msg4..msg5=old tool output"
pcodx current-session-message-ids --session work
pcodx resume --session work
pcodx resume --last --text "continue from the compacted future context"
pcodx interactive --session work
```

`resume` renders stored compacted context, so it is not an empty session. The storage detail is an implementation detail of the prototype, not the product framing.

Commands:

- `init`: create or refresh a pcodx wrapper session
- `turn`: record an exact human prompt and render future context
- `record`: record a system, developer, assistant, tool, or user entry
- `compact`: replace a visible `msg...` or `cmp...` range with a summary
- `compact-many`: atomically replace multiple disjoint visible ranges with summaries
- `ids`: list visible range endpoints
- `current-session-message-ids`: print the shared agent helper text for endpoint selection
- `show`: render current future context
- `resume`: render an existing session and optionally append a human prompt
- `interactive`: open a Codex-like line interface; plain text records a user turn, and slash commands include `/record`, `/compact`, `/ids`, `/show`, `/current-session-message-ids`, and `/exit`
- `prompts`: list or print shared prompt fragments
- `serve`: run a Codex TUI to Codex app-server proxy, optionally wiring PCODX tools at websocket JSON-RPC text-message boundaries with `--enable-pcodx-tools`

`--text` is one exact CLI string. `--text-file PATH` reads exact text from a file, and `--text-file -` reads stdin. This avoids joining separate argv words, which can alter whitespace.

For `pcodx interactive`, the optional initial prompt supports `--text` or `--text-file PATH`; it rejects `--text-file -` because stdin is reserved for the interactive command loop.

In this CLI, `input` means the bytes pcodx records for one turn. For `turn` and `resume --text...`, that input is a human prompt. For `record`, it can be a system, developer, user, assistant, or tool message.

`rendered context` means the future Codex context after applying compactions. Preserved turns are printed verbatim with an appended id marker like `<aboveturn id="msg1"/>`; compacted ranges are printed as summaries with ids like `<aboveturn id="cmp1"/>`.

KV-cache reuse means reusing a model server's cached computation for an unchanged prefix of a conversation. The intended wrapper keeps that prefix stable except for the two required mutations above. The current prototype stores original turn text unchanged, appends ids only in rendered context, and replaces only compacted ranges with summaries.

`dynamic tools` means tools registered with a future app-server session at runtime, such as partial-compaction tools the model could call. It does not mean redefining slash commands in this CLI prototype.

`pcodx interactive` is the local Codex-like CLI path for this prototype. It uses the same durable store and validation as `record`, `compact`, and `show`, so it can perform partial compaction without live websocket fixture capture. It is intentionally a local command loop, not a replacement for the real Codex TUI proxy.

## demo

Run the Codex-like partial-compaction demo in tmux:

```sh
scripts/pcodx_codex_like_demo.sh
tmux attach -t pcodx-codex-like-demo
```

The pane opens `pcodx interactive`, reads three files through that frontend, compacts the beginning and ending file reads, keeps the middle file read visible, records forgotten-vs-retained future-query prompts, exits, resumes, and repeats the rendered-context checks after resume. This proves PCODX-rendered future context forgets and retains selectively. It does not prove live model recall because the current Rust proxy cannot yet route the next native Codex turn through a fresh upstream app-server thread seeded only with the compacted ledger render. The durable demo requirement is in `docs/demo.md`.

Run the real Codex middleware path in tmux:

```sh
scripts/pcodx_real_codex_proxy_demo.sh
tmux attach -t pcodx-real-codex-proxy-demo
```

The left pane is `pcodx serve` with PCODX dynamic tools enabled against an isolated demo database. The right pane is real Codex TUI connected through `--remote ws://127.0.0.1:48570`. After `/exit`, the script runs `codex resume --last` through the same proxy.

As of Codex CLI 0.142.4, upstream `codex app-server` may log `failed to decode models response: missing field models` while receiving an OpenAI-compatible `{"object":"list","data":[...]}` model list. The same log appears when running `codex app-server --listen ...` without `pcodx`, so this is an upstream model-list schema mismatch, not a proxy decode failure.

## install

Build/install with:

```sh
git submodule update --init --recursive
cargo install --path . --locked --root ~/.local
```

This installs `pcodx` to `~/.local/bin` if that directory is on `PATH`.

## storage

Default prototype database: `$PCODX_DB`, else `$XDG_DATA_HOME/pcodx/pcodx.sqlite3`, else `~/.local/share/pcodx/pcodx.sqlite3`.

Tables:

- `sessions`: session id, working directory, update time, upstream Codex id placeholder, and `kv_cache_boundary`
- `messages`: full message text, role, stable `msg1` ids, source, and a human-prompt flag
- `compactions`: stable `cmp1` ids, covered message range, summary, and replacement count

Human prompts are stored exactly as supplied. Compaction allows any range, including user prompts that contain bulky logs. If the range includes system, developer, or user messages, `pcodx compact` prints a warning that the summary must preserve active instructions and human intent.

Validation rejects ranges that split an assistant/tool pair. If a range ends on the assistant turn immediately before a tool result, the error says to extend to the tool turn. If a range starts on that tool result, the error says to include the assistant turn or start after the tool turn. This ports the OpenCode POC's tool-use/tool-result boundary rule into the Rust turn model.

## tool endpoint shape

`src/tool_endpoint.rs` contains the Codex-facing tool shape over the same storage core:

- `partial_compact_json(store, session_id, args_json, config)` parses OpenCode-style `ranges`, rejects cross-session selectors, truncates long summaries, calls `compact_ranges`, and returns an OpenCode-style JSON result
- `current_session_message_ids_tool(store, session_id)` returns the shared current-session ID helper text for endpoint selection

The endpoint layer does not rewrite stored history. It records only compaction summaries and leaves original messages available for history/recovery.

## kv-cache boundary

The app-server wrapper should not change Codex context except by appending ids after completed turns and replacing compacted ranges with summaries. The TypeScript Codex wrapper proof-of-concept that demonstrates this design is documented in `docs/codex-wrapper-poc.md`. The Rust proxy currently has only the first concrete JSON-RPC hook point when messages are visible as complete websocket text messages:

- opt-in client request boundary: with `--enable-pcodx-tools`, websocket text JSON-RPC `thread/start`, `thread/resume`, and `thread/fork` params are augmented with `dynamicTools` entries for `partial_compact`, `partial_compact_current_session_message_ids`, and `partial_compact_instructions`
- server request boundary: websocket text JSON-RPC upstream `item/tool/call` requests for those tools are answered inside the proxy and routed to `src/tool_endpoint.rs`
- fallback behavior: non-text websocket messages and non-PCODX JSON-RPC text messages are relayed as websocket messages
- current session binding: tool calls route to the single PCODX session selected when `pcodx serve` starts
- current storage source: `serve` does not yet ingest native Codex thread history into PCODX storage; tool calls operate only on messages already recorded in the selected PCODX session
- current live-context source: PCODX dynamic tools can read and write PCODX compaction state during one `serve` process, but the Rust proxy does not yet start fresh upstream threads from only the compacted ledger render
- fixture capture: set `PCODX_WS_FIXTURE_DIR` when running `pcodx serve` to write observed websocket text JSON-RPC messages as numbered JSON files for protocol verification; this works without enabling PCODX tool injection

The current remaining Rust integration blocker is porting the Codex wrapper proof's thread mapping and fresh-upstream-turn path. The concrete observable boundary is websocket text JSON-RPC: transparent proxying forwards native Codex traffic; dynamic tool execution can mutate PCODX state for one selected wrapper session; native history ingestion is the point where Codex user, assistant, and tool items become PCODX `msg...` rows; fresh upstream thread creation plus compacted ledger injection is the point where future Codex app-server turns receive only the compacted view. Websocket fixture capture is useful evidence for parsing event boundaries, but it is not a blocker for the native frontend -> PCODX proxy -> native app-server proof-of-concept path.

## prompt source

`vendor/agent_partial_compact_common` is a Git submodule containing shared partial-compaction prompt fragments. The test suite checks embedded prompt bytes against that submodule.

## deferred

- native Codex session id mapping
- native item ingestion and fresh upstream future-turn replacement in the Rust proxy path

Historical JSON migration is a non-goal. Those files were evidence for earlier experiments; this prototype starts with a clean durable history.
