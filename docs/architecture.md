pcodx architecture

- goal
  - Codex-like CLI with partial compaction
  - small Rust codebase
  - one SQLite database for durable wrapper state

- runtime disk layout
  - `$PCODX_DB`
    - explicit SQLite database path
  - `$XDG_DATA_HOME/pcodx/pcodx.sqlite3`
    - default with XDG data home
  - `~/.local/share/pcodx/pcodx.sqlite3`
    - default fallback
  - `target/`
    - Rust build output
    - ignored

- storage model
  - `sessions`
    - pcodx wrapper session
    - spans one or more Codex sessions
  - `messages`
    - exact role text
    - simple `msg1` ids
    - system, developer, and user roles trigger preserve warnings on compaction
  - `compactions`
    - simple `cmp1` ids
    - selected message range
    - replacement summary

- render model
  - preserved turn text is emitted unchanged
  - each visible entry ends with one marker
    - `<aboveturn id="msg1"/>`
    - `<aboveturn id="cmp1"/>`
  - compacted ranges render only their summary plus `cmp` marker

- prompt source
  - prompt fragments live in `agent_partial_compact_common`
  - this repo consumes them through `vendor/agent_partial_compact_common`
  - public remote is `https://github.com/SichangHe/agent_partial_compact_common`
  - tests compare embedded bytes with the submodule files

- dynamic tool boundary
  - dynamic tool registration means the future app-server proxy advertises tools such as `partial_compact` and current-id lookup to Codex
  - this skeleton has the storage and CLI behavior behind those future tools
  - it does not yet start or proxy Codex app-server
