rust rewrite design

- goal
  - small Codex-like partial-compaction wrapper
  - one SQLite database for durable state
  - no proof-of-concept run-tree sprawl

- old junk replaced
  - `node_modules`
    - dependency install output, not runtime state
  - `dist`
    - TypeScript build output
  - `runs/*`
    - JSON ledgers, context text files, reports, smoke receipts
  - `pcodx-ledger.json`
    - sidecar-only working-memory ledger
  - `/tmp/pc-poc.log`
    - early hook probe log

- new disk layout
  - `target/`
    - ignored Rust build output
  - `pcodx.sqlite3`
    - sessions, messages, compactions
  - `pcodx.sqlite3-wal` and `pcodx.sqlite3-shm`
    - SQLite journal files when enabled by SQLite

- session model
  - `sessions`
    - stable wrapper session id
    - cwd and update time
    - placeholder upstream Codex id
    - `kv_cache_boundary=future_turn_only`
  - `messages`
    - full role text
    - stable `msg000001` ids
    - system, developer, and user messages marked as preserved
  - `compactions`
    - stable `cmp000001` ids
    - message-id range and summary

- prompt preservation
  - OpenCode prompt fragments are vendored as files
  - tests compare bytes against source prompts when present
  - user role text is stored exactly
  - compaction refuses ranges containing system, developer, or user messages

- kv-cache policy
  - live hidden transcript mutation is out of scope
  - rendering is only for future turns
  - preserved text stays before PCODX markers
  - compaction changes only later fresh-thread seed context

- implemented now
  - `init`, `record`, `turn`, `resume`, `ids`, `show`, `compact`, `prompts`
  - SQLite migration and persistence
  - visible-context render from SQLite
  - tests for storage and prompt byte preservation

- deferred
  - real app-server proxy
  - dynamic tools
  - native Codex resume mapping
  - importing old JSON ledgers
