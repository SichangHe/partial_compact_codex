cleanup behavior

- old proof-of-concept garbage
  - ignored `node_modules/`
  - ignored `dist/`
  - ignored experiment dependency trees
  - root `.opencode/node_modules/`

- cleaned safely
  - dependency/build output can be regenerated
  - tracked experiment evidence is retained
  - current follow-up removed ignored proof-of-concept dependency/build directories from `opencode_partial_compact`

- pcodx data retention
  - pcodx does not automatically delete sessions
  - automatic deletion would risk losing recovery history
  - SQLite can be compacted with ordinary SQLite tools after deliberate deletion policy exists

- not cleaned automatically
  - Rust `target/`
    - local build cache
    - not old proof-of-concept state
  - tracked proof-of-concept `runs/`
    - retained as evidence
  - `opencode_partial_compact/.opencode/package.json` and package lock
    - retained because they are install metadata, not large dependency payload
  - `opencode_partial_compact/experiments/poc`
    - retained as prior-art source and notes
    - its `/tmp/pc-poc.log` behavior is historical POC documentation
