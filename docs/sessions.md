pcodx sessions

- pcodx session id
  - wrapper-owned id returned by `pcodx init`
  - names durable wrapper state
  - survives partial compaction

- Codex session id
  - upstream Codex-owned id
  - `pcodx serve` preserves the upstream session by relaying the real Codex frontend to the real app-server
  - the target proxy keeps the active Codex session when the app-server API can accept the allowed context changes
  - a new upstream session is only a fallback when current Codex APIs cannot replace the compacted range in place

- why ids differ
  - pcodx owns compaction history
  - Codex owns native transcript and UI state
  - the wrapper maps its durable session to whichever upstream Codex session is active

- partial-compaction session handling
  - compaction does not create a new pcodx session
  - the intended app-server proxy preserves the upstream Codex session and applies only the allowed context changes
  - current dynamic tool calls can update the selected pcodx session during one `serve` process
  - current `serve` does not ingest native Codex history into that pcodx session
  - current Codex app-server exposes no confirmed in-place partial replacement RPC for the active thread context
  - the exact blocker is applying PCODX-rendered compacted context to the native live Codex thread while preserving as much KV cache as possible

- rollback
  - correct future behavior is to resume the previous Codex session at the rollback point
  - pcodx then records a new branch mapping from that Codex session into the same wrapper session
  - this prototype has no native Codex rollback command because it does not yet own upstream Codex sessions
