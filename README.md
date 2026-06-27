# partial_compact_codex

Rust rewrite skeleton for a Codex-like partial-compaction wrapper.

The proof-of-concept stored JSON ledgers, visible-context text files, per-turn reports, smoke receipts, build output, and `node_modules` inside the wrapper tree. This repo starts with one durable runtime artifact instead: a SQLite database. Build output stays under `target/` and is ignored.

## cli shape

```sh
pcodx init --session work
pcodx turn --session work --text "first human prompt"
pcodx record --session work --role assistant --text "large stale discovery"
pcodx compact --session work --from msg000002 --to msg000002 --summary "discovery was stale; durable fact ..."
pcodx resume --session work
pcodx resume --last --text "continue from the compacted future context"
```

`resume` renders stored compacted context from SQLite, so it is not an empty session. This first skeleton does not yet launch or proxy the real Codex app-server.

## storage

Default database: `$PCODX_DB`, else `$XDG_DATA_HOME/partial_compact_codex/pcodx.sqlite3`, else `~/.local/share/partial_compact_codex/pcodx.sqlite3`.

Tables:

- `sessions`: session id, working directory, update time, upstream Codex id placeholder, and `kv_cache_boundary`
- `messages`: full message text, role, stable `msg000001` ids, source, and a human-prompt flag
- `compactions`: stable `cmp000001` ids, covered message range, summary, and replacement count

Human prompts are stored exactly as supplied. The current compaction command refuses ranges containing system, developer, or user messages, which is conservative and keeps instruction/prompt preservation easy to audit.

## kv-cache boundary

PCODX should not try to mutate a live hidden Codex transcript. The safe boundary is future-turn rendering: preserved messages are emitted in original order and annotated after their text, while compacted ranges are replaced by summaries. A future app-server integration should seed a fresh upstream thread with this render after compaction.

## prompt source

`assets/prompts/` vendors the OpenCode partial-compaction prompt fragments verbatim. The test suite checks them against `/ssd1/sichangheagent/opencode_partial_compact/src/prompts` when that source tree exists.

## deferred

- real Codex app-server proxy/front-end launch
- dynamic tool registration
- native Codex session id mapping
- compaction ranges using visible `cmp...` ids as endpoints
- transcript import from existing proof-of-concept JSON ledgers
