kv-cache boundary

- fact
  - pcodx cannot safely rewrite a live hidden Codex transcript today

- rule
  - compaction changes only future context rendered from the pcodx ledger
  - a future app-server proxy should seed a fresh Codex session after compaction

- marker placement
  - preserved text comes first, byte-for-byte
  - the id marker is appended after the visible turn
  - markers are minimal
    - `<aboveturn id="msg1"/>`
    - `<aboveturn id="cmp1"/>`

- cache implication
  - new markers are appended only when a turn first enters pcodx-rendered future context
  - existing preserved turn text is not edited
  - compacting a range intentionally changes that range and later prompt prefix
  - pcodx does not claim native KV-cache reuse across a fresh compacted Codex session

