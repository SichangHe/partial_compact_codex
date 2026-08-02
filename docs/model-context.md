model-visible context

- current failure at commit `298c608`
  - `pcodx compact` changes the Rust SQLite ledger and `show` render
  - `pcodx serve` registers dynamic tools and returns their results
  - the proxy forwards the same native Codex thread after compaction
  - the next native model request therefore keeps the old hidden history

- discarded Rust history
  - commits `bcc6b1a` and `e1542a3` injected changed renders into resumed threads
  - that path appended the new render after old native history
  - append-only reseeding could increase context but could not remove the selected range
  - commit `298c608` removed it and left no actual model-context mutation

- Codex 0.146.0 boundary
  - `thread/inject_items` only appends model-visible items
  - `thread/rollback` only drops a suffix and is deprecated
  - `thread/compact/start` accepts only a thread id
  - no app-server request replaces an arbitrary active-history range with caller-supplied text
  - official reference: `https://developers.openai.com/codex/app-server`

- Rust controller path
  - `pcodx-model-turn` reads the current visible Rust ledger entries
  - it preserves visible system, developer, user, and assistant roles and order
  - a compaction summary inherits the strongest covered authority
    - system, then developer, then user, then assistant
  - a visible flat tool row is rejected
    - the SQLite row lacks the call id needed for a raw Responses API tool-output item
    - compact it with its assistant call before the next model turn
  - it creates a fresh ephemeral Codex app-server thread
  - it injects the render with `thread/inject_items`
  - it sends the follow-up prompt with `turn/start`
  - it does not advertise compaction tools during that one turn
    - run `pcodx compact` between controller turns
  - it requires `thread/tokenUsage/updated` before reporting success
  - it atomically appends the completed user, native tool items, and assistant turn
  - each appended row source retains the upstream thread and turn ids
  - compacted originals remain recoverable in SQLite
  - stable `msg` and `cmp` ids retain existing replacement semantics

- cache boundary
  - every controller turn uses a fresh upstream thread
  - active-thread KV-cache reuse is not claimed
  - provider-side unchanged-prefix caching may occur but is not guaranteed by PCODX

- use
  - create and populate a session with `pcodx`
  - compact selected ids with `pcodx compact`
  - run the actual next model turn
    - `pcodx-model-turn --session work --text "continue"`
    - the stored session working directory is the default
    - `--cwd DIR` explicitly overrides it
  - request machine-readable token evidence
    - `pcodx-model-turn --session work --text "continue" --json`
  - preserve the exact injected item array for audit
    - add `--context-out PATH`
    - the path must not already exist

- regression proof
  - focused fake-app-server test
    - `cargo test model_context::tests::next_model_payload_and_reported_tokens_shrink_after_compaction`
    - fails unless the second `thread/inject_items` payload omits raw sentinels
    - fails unless the payload contains the compacted summary
    - fails unless reported next-turn input tokens shrink
  - authenticated live proof
    - `cargo test model_context::tests::live_next_model_turn_sees_smaller_compacted_context -- --ignored --nocapture`
    - performs one baseline model turn and one post-compaction follow-up turn
    - rejects either turn if the model runs a tool to recover the phrase from disk
    - writes both injected contexts and `result.json` under `target/pcodx-context-proof`

- remaining frontend gap
  - `pcodx serve` still lacks durable native-thread to Rust-ledger mapping
  - it also lacks completed-item ingestion and next-turn routing to a fresh upstream thread
  - `pcodx-model-turn` is the working Rust controller boundary, not an in-place TUI history rewrite
