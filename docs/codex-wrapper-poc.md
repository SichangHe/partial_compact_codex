codex wrapper proof-of-concept

- source tree
  - `/ssd1/sichangheagent/opencode_partial_compact/experiments/codex-wrapper`
  - this is the Codex proof source for Rust rebuild work
  - OpenCode plugin transform docs are background, not the primary source

- entry points
  - `bin/pcodx`
    - user-facing wrapper command
  - `src/frontend-proxy.ts`
    - Codex CLI frontend to Codex app-server websocket proxy
  - `src/self-compacting-controller.ts`
    - controller-owned Codex app-server turn runner
  - `src/app-server-adapter.ts`
    - stdio JSON-RPC adapter for `codex app-server`
  - `src/ledger.ts`
    - durable wrapper ledger and render logic
  - `src/controller-cli.ts`
    - REPL and command path for controller-owned sessions
  - `src/verify-self-compaction.ts`
    - acceptance verifier

- ledger data
  - `schema_version`
  - `session_id`
  - `messages`
    - stable `msg...` id
    - role
    - text
    - creation timestamp
    - optional source
  - `compactions`
    - stable `cmp...` id
    - `from_message_id`
    - `to_message_id`
    - summary
    - creation timestamp
    - replaced message count
  - `visible_message_ids`
    - current render endpoints after compactions

- render rule
  - preserved messages render as original text plus `<aboveturn id="msg..."/>`
  - compacted ranges render as summary plus `<aboveturn id="cmp..."/>`
  - original compacted message text remains in the ledger
  - original compacted message text is absent from the rendered future context

- context mutation
  - the controller owns future app-server turns
  - each future turn starts from `WrapperLedger.renderVisibleContext`
  - after `partial_compact`, the next turn uses the compacted render
  - compacting during a turn does not shrink that already-running turn
  - the proxy path invalidates mapped upstream threads after a successful compaction
  - the next turn starts a fresh upstream app-server thread
  - the fresh thread receives only the current compacted ledger render

- Codex CLI and app-server interception
  - native Codex CLI still owns slash commands, TUI, approval UX, review UI, and resume command parsing
  - PCODX starts a real upstream `codex app-server`
  - PCODX places a websocket proxy between Codex CLI `--remote` and the upstream app-server
  - client `thread/start`, `thread/resume`, and `thread/fork` params are augmented with PCODX dynamic tools
  - upstream `item/tool/call` requests for PCODX tools are answered by the proxy
  - ordinary upstream requests are forwarded to the native frontend
  - ordinary completed Codex items are rendered into ledger messages on non-compacting turns
  - PCODX dynamic tool calls are mirrored back to the native frontend as completed dynamic-tool items

- proven by the demo and verifier
  - controller-owned future app-server turns can be seeded from compacted ledger context
  - app-server token usage shrinks after compaction in the context-shrink smoke
  - dynamic `partial_compact` mutates the ledger during a Codex app-server turn
  - the following app-server turn omits compacted-away sentinel text
  - front-end proxy protocol smoke verifies dynamic tool advertisement, thread mapping, invalidation, fresh-upstream routing, and future-context shrink with a fake upstream
  - front-end proxy source is designed to preserve native Codex UI routing while controlling upstream future context

- not proven
  - front-end proxy smoke is not a real model-call proof
  - a stock Codex CLI thread hidden transcript is not rewritten in place
  - an arbitrary native Codex session UUID cannot reconstruct a missing PCODX ledger
  - a running model call is not shrunk mid-turn
  - append-only `thread/inject_items` is not sufficient as the partial-compaction mechanism

- resume behavior
  - native `pcodx resume ...` delegates to Codex resume through the proxy
  - PCODX maps the working directory to the latest matching wrapper run when `--run-dir` is absent
  - resume is durable when the PCODX ledger and native Codex run mapping are available
  - resume cannot recover compacted state from only an arbitrary native Codex session id

- Rust file disposition
  - keep `src/storage.rs`
    - it already models messages, compactions, visible ids, validation, and rendering
  - keep `src/tool_endpoint.rs`
    - it exposes the dynamic tool shape over storage
  - keep `src/prompts.rs`
    - it embeds the shared prompt fragments
  - rewrite `src/proxy.rs`
    - current dynamic-tool registration and tool-call handling are useful
    - old append-only seed/reseed code is discarded
    - missing work is thread mapping, native item ingestion, invalidation, fresh upstream thread start, and compacted render injection
  - rewrite `scripts/pcodx_real_codex_proxy_demo.sh`
    - it must test the fresh-upstream future-context path once implemented
  - keep `pcodx interactive`
    - it remains a local storage/render demo only
