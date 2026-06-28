kv-cache boundary

- product rule
  - preserve Codex context exactly where possible
  - append turn ids only after completed turns
  - replace compacted ranges only with the agent's summary

- current prototype fact
  - `pcodx serve` can sit between real Codex TUI and real Codex app-server
  - the live proxy currently relays protocol bytes unchanged
  - the live proxy does not block native Codex client mutations
  - pcodx cannot safely rewrite a live hidden Codex transcript today
  - compaction changes only future context rendered by the prototype
  - Codex app-server 0.142.3 exposes no documented in-place arbitrary range replacement API

- marker placement
  - preserved text comes first, byte-for-byte
  - the id marker is appended after the visible turn
  - markers are minimal
    - `<aboveturn id="msg1"/>`
    - `<aboveturn id="cmp1"/>`

- cache implication
  - new markers are appended after a turn enters pcodx-rendered future context
  - existing preserved turn text is not edited
  - compacting a range intentionally changes that range and later prompt prefix
  - transparent live proxying preserves Codex bytes
  - pcodx does not claim native KV-cache reuse across a live compacted Codex session
