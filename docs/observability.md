pcodx usage log

- prior durable state
  - SQLite preserved PCODX messages and compactions but not Codex token usage
  - `--context-out` preserved the full injected context only when requested
    - that content-bearing audit file is not a safe aggregate usage log
  - Codex `log_dir` enables a plaintext TUI log and Codex OTel requires an exporter for persistence
    - `https://developers.openai.com/codex/config-advanced`
  - PCODX therefore persists a narrow JSONL record at the app-server usage-event boundary

- supported boundary
  - `pcodx-model-turn` appends one record after each successful model turn
  - the compaction snapshot is taken before the new prompt is sent
  - `pcodx serve` is excluded because it does not yet route compacted state into the next native turn

- persistent location
  - `--usage-log PATH` wins
  - `$PCODX_USAGE_LOG` is next
  - otherwise `DB.usage.jsonl` is beside the selected SQLite database
    - default database `pcodx.sqlite3` produces `pcodx.usage.jsonl`
  - each compact JSON object is appended as one line and synced before the command reports success

- correlation boundary
  - one line is one completed upstream model turn
  - `pcodx_session_correlation_id` joins lines for the same database and PCODX session without storing either path or session name
    - PCODX generates this random opaque id once and stores it with the session in SQLite
  - `upstream_thread_id` and `upstream_turn_id` join it to Codex events
  - compaction counts identify baseline records and records made after partial compaction
  - the Codex and PCODX versions identify the implementation used during a trial

- measured fields
  - `usage.input_tokens` is Codex `tokenUsage.last.inputTokens`
  - `usage.cached_input_tokens` is Codex `tokenUsage.last.cachedInputTokens`
  - `usage.uncached_input_tokens` is input minus cached input, floored at zero
    - this matches Codex 0.146.0 TUI's `non_cached_input` calculation
    - `https://github.com/openai/codex/blob/rust-v0.146.0/codex-rs/tui/src/token_usage.rs`
  - `usage.model_context_window_tokens` is Codex `tokenUsage.modelContextWindow`
  - output, reasoning, total, and cache-write token counts are retained for interpretation
  - injected item count and serialized JSON byte count are safe context-size proxies
  - envelope fields are `schema_version`, `event`, `recorded_at_unix_ms`, `pcodx_version`, `pcodx_session_correlation_id`, `codex_version`, `upstream_thread_id`, and `upstream_turn_id`
  - nested objects are `compaction`, `context`, and `usage`

- privacy boundary
  - records contain aggregate counts, software identifiers, one random opaque session correlator, and upstream ids
  - records omit prompts, replies, summaries, session names, message ids, tool data, working directories, database paths, auth data, environment values, and the full Codex user agent
  - the log is created with mode `0600` on Unix when it does not already exist
  - an existing Unix log with group or world permissions is rejected before a record is written
  - a log path that is the database or a hard-link alias of it is rejected

- honest limits
  - values are provider-reported counts observed through Codex, not PCODX token estimates
  - `tokenUsage.last` describes Codex's latest response usage update
    - a turn with several model calls cannot be reconstructed call by call from this event
  - `modelContextWindow` is nullable in the installed schema and remains JSON `null` when Codex omits it
  - cached input proves the provider reported a cache hit
    - it does not prove reuse of one native Codex thread because the working controller starts a fresh upstream thread
  - failed or interrupted turns do not produce a completed-turn record

- verified contract and earlier conflict
  - token usage
    - verified fact: official app-server documentation says "`thread/tokenUsage/updated` - usage updates for the active thread"
      - `https://developers.openai.com/codex/app-server`
    - verified fact: Codex 0.146.0 source and its generated schema specify `inputTokens`, `cachedInputTokens`, `modelContextWindow`, and the other recorded fields
      - `https://github.com/openai/codex/blob/rust-v0.146.0/codex-rs/app-server-protocol/src/protocol/v2/thread.rs`
    - conclusion: these facts do not conflict
      - the public page names the event but does not state its payload shape
      - the installed schema supplies that detail
  - model list
    - verified fact: the public Models API documents `{ "object": "list", "data": [...] }`
      - `https://developers.openai.com/api/reference/resources/models/methods/list`
    - verified fact: Codex 0.146.0 deserializes its product catalog as `{ "models": [...] }`
      - `https://github.com/openai/codex/blob/rust-v0.146.0/codex-rs/protocol/src/openai_models.rs`
    - inference: an OpenAI-compatible provider can answer successfully yet fail Codex's product-catalog decoder
    - conclusion: the model-list warning is upstream of PCODX and unrelated to token-usage logging
