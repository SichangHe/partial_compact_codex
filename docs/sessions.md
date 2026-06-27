pcodx sessions

- pcodx session id
  - wrapper-owned id returned by `pcodx init`
  - names the durable SQLite ledger
  - survives partial compaction

- Codex session id
  - upstream Codex-owned id
  - future app-server integration may create many Codex sessions for one pcodx session
  - after partial compaction, the safe design is to start a fresh Codex session seeded from the compacted pcodx render

- why ids differ
  - pcodx owns compaction history
  - Codex owns native transcript and UI state
  - one pcodx session may need multiple Codex sessions because compacted future context is a new seed

- partial-compaction session creation
  - compaction does not create a new pcodx session
  - compaction should create a new upstream Codex session in the future proxy
  - if a run truly needs one native Codex session only, pcodx can store that Codex id as the only upstream session for the wrapper session

- rollback
  - correct future behavior is to resume the previous Codex session at the rollback point
  - pcodx then records a new branch mapping from that Codex session into the same wrapper session
  - this skeleton has no native Codex rollback command because it does not yet own upstream Codex sessions

