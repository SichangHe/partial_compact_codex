OpenCode mechanism

- core mechanism
  - the model sees a transformed current-session message list
  - the durable OpenCode session log stays unchanged
  - partial compaction records live in a plugin sidecar
  - each record names an inclusive message-id range
  - each record stores the agent-written replacement summary
  - the next LLM request replaces that range with one synthetic text part

- proven OpenCode boundary
  - implemented by `src/hook.ts`
  - hook name is `experimental.chat.messages.transform`
  - handler is `messagesTransformHandler`
  - it receives outbound messages before provider submission
  - it mutates `output.messages` in place
  - this is live for OpenCode's next provider call
  - source phrase from `experiments/poc/README.md`
    - "in-place mutation of the messages array works"

- replacement behavior
  - `applyCompactions` scans records in stored order
  - unresolved records are skipped
  - first message in the range survives
  - surviving message parts are replaced with one synthetic text part
  - interior messages are removed from the array
  - synthetic text format
    - `[compacted: session <session_id>: <from>..<to> — <summary>]`
  - synthetic part id format
    - `pc_<from_message_id>`
  - synthetic source marker
    - `opencode-partial-compact`

- non-replacement behavior
  - original message rows are not edited
  - OpenCode UI history is not rewritten
  - compacted text is not deleted from disk
  - compaction affects the model-visible outbound view

- tool surface
  - implemented by `src/tool.ts`
  - `partial_compact_instructions`
    - returns detailed instructions from shared prompt fragments
  - `partial_compact_current_session_message_ids`
    - returns visible current-session ids after active compactions
  - `partial_compact`
    - accepts `ranges`
    - rejects empty or incomplete ranges
    - rejects cross-session selectors
    - truncates summaries by config
    - validates ranges against current OpenCode messages
    - persists records
    - returns JSON receipt

- validation rules
  - implemented by `src/validate.ts`
  - endpoints must exist
  - from endpoint must not come after to endpoint
  - new range must not overlap active records
  - requested ranges must not overlap each other
  - range must not include an existing synthetic compaction part
  - range must not split a tool-use/tool-result pair

- storage model
  - implemented by `src/state.ts`
  - default sidecar directory
    - `~/.local/share/opencode/storage/plugin/opencode-partial-compact`
  - one JSON file per session id
  - `schema_version`
    - currently `1`
  - `session_id`
    - OpenCode session id
  - `compactions`
    - sorted records
  - `last_reminder`
    - visible-token reminder baseline
  - `last_written_iso`
    - update timestamp
  - writes are atomic through temp file plus rename
  - corrupt sidecars are backed up as `.bad-*`
  - newer schema versions fail closed

- compaction record
  - `session_id`
    - optional on record type
  - `from_message_id`
    - inclusive start
  - `to_message_id`
    - inclusive end
  - `summary`
    - agent-authored replacement
  - `created_at_iso`
    - record timestamp
  - `n_messages_replaced`
    - validation-derived count

- resume behavior
  - first transform after process restart calls `warmCache`
  - `warmCache` reloads the sidecar for the current session
  - `getCompactionsSync` then supplies active records
  - same sidecar records produce the same model-visible replacement
  - durable resume depends on unchanged OpenCode message ids

- native compaction coexistence
  - OpenCode native auto-compaction is disabled by config hook
  - source module is `src/plugin.ts`
  - reason
    - native auto-compaction schedules from stale token records
  - overflow native compaction is allowed as fallback
  - stale plugin records can be pruned after native compaction when both endpoints vanished

- what was not proven for Codex
  - no stock Codex CLI hidden transcript is rewritten by this OpenCode code
  - no arbitrary active Codex thread middle-range deletion is demonstrated
  - the proven live replacement API is OpenCode-specific

