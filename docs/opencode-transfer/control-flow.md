OpenCode control flow

- plugin bootstrap
  - `src/plugin.ts` exports `server`
  - config is loaded from project or user config
  - debug log path is configured
  - native auto-compaction is disabled when plugin is enabled
  - coexistence check is built but deferred
  - tools and experimental hooks are returned to OpenCode

- config path
  - project config
    - `.opencode/opencode-partial-compact.jsonc`
  - user config
    - `~/.config/opencode/opencode-partial-compact.jsonc`
  - defaults
    - enabled
    - 2000 summary chars
    - reminders enabled
    - 16000-token reminder interval

- agent-driven compaction path
  - agent calls `partial_compact_current_session_message_ids`
  - tool loads current OpenCode session messages
  - tool applies existing records to a copy
  - helper returns visible ids
  - agent selects one or more ranges
  - agent writes faithful summaries
  - agent calls `partial_compact`
  - tool fetches current session messages
  - sidecar records are loaded
  - ranges are validated
  - records are persisted
  - reminder baseline is recalculated on compacted visible view
  - tool returns JSON receipt
  - current tool call does not retroactively shrink the already-running model call
  - subsequent provider calls use the transformed message list

- outbound model-call path
  - OpenCode builds messages for a provider call
  - `experimental.chat.messages.transform` fires
  - coexistence check runs once
  - sidecar cache is warmed
  - active records are read synchronously from cache
  - `applyCompactions` mutates outbound messages
  - provider receives the compacted view

- system reminder path
  - `experimental.chat.system.transform` fires
  - current session messages are fetched
  - existing records are applied to a copy
  - visible tokens are estimated from compacted parts
  - reminder fires when interval or usage band is crossed
  - reminder is appended to system text
  - sidecar baseline is updated

- TUI slash-command path
  - TUI plugin is `src/tui.ts`
  - command is `/partial_compact`
  - command requires the current route to be a session
  - TUI reads messages from session state
  - TUI reads parts from session state
  - sidecar is loaded fresh
  - first compactable message starts after active compacted ranges
  - checkpoint choices are built from messages and notable parts
  - invalid endpoints are filtered through the same validation rule
  - user selects a checkpoint
  - TUI sends a prompt into the session
  - prompt tells the agent exact `from_message_id` and `to_message_id`
  - prompt points to `partial_compact_instructions`
  - agent still performs summarization and calls the tool

- current-session ids after compaction
  - visible original message ids remain selectable
  - ids hidden by active compactions are omitted
  - compacted synthetic ids are not first-class OpenCode range endpoints in this implementation
  - Rust PCODX may expose `cmp...` endpoints, but that is an extension

- failure behavior
  - validation errors return JSON error text to the agent
  - persistence failures leave the in-memory cache unchanged
  - corrupt sidecars are backed up and replaced with empty state
  - newer sidecar schemas stop the plugin
  - disabled plugin returns tool errors and no-op hooks

