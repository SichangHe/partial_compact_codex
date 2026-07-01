OpenCode partial-compaction transfer

- status
  - the proof of concept is understood
  - OpenCode proves live outbound model-context replacement inside OpenCode
  - the Rust repo currently has only partial equivalents
  - PCODX must implement the same ownership boundary with Codex app-server threads

- source tree
  - OpenCode source repo
    - `../../../opencode_partial_compact`
  - Rust target repo
    - current repo
  - shared prompts
    - `vendor/agent_partial_compact_common` from each repo root

- document tree
  - `mechanism.md`
    - core behavior
    - data model
    - replacement semantics
    - resume semantics
  - `control-flow.md`
    - agent tool path
    - transform hook path
    - reminder path
    - TUI slash-command path
  - `user-visible-path.md`
    - exact OpenCode demo path
    - exact Codex-wrapper evidence path
    - honest user-visible limits
  - `rust-transfer.md`
    - OpenCode-to-PCODX component map
    - implementation requirements
    - current Rust gaps

- essential source phrases
  - OpenCode `README.md`
    - "partial_compact(...) lets the agent replace one contiguous current-session message range"
    - "only the in-memory view sent to"
  - OpenCode `experiments/poc/README.md`
    - "in-place mutation of the messages array works"
  - Codex wrapper `experiments/codex-wrapper/README.md`
    - "the current turn still contains its original prompt history"
    - "the next controller-started app-server turn is smaller"
