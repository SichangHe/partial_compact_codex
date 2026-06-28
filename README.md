# pcodx

Rust rewrite skeleton for a Codex-like partial-compaction wrapper.

The proof-of-concept stored JSON ledgers, visible-context text files, per-turn reports, smoke receipts, build output, and `node_modules` inside the wrapper tree. This repo starts with one durable runtime artifact instead: a SQLite database. Build output stays under `target/` and is ignored.

## cli

```sh
pcodx init --session work
pcodx turn --session work --text "first human prompt"
pcodx turn --session work --text-file prompt.md
pcodx record --session work --role assistant --text "large stale discovery"
pcodx compact --session work --from msg2 --to msg2 --summary "discovery was stale; durable fact ..."
pcodx resume --session work
pcodx resume --last --text "continue from the compacted future context"
```

`resume` renders stored compacted context from SQLite, so it is not an empty session. This first skeleton does not yet launch or proxy the real Codex app-server.

Commands:

- `init`: create or refresh a pcodx wrapper session
- `turn`: record an exact human prompt and render future context
- `record`: record a system, developer, assistant, tool, or user entry
- `compact`: replace a visible `msg...` or `cmp...` range with a summary
- `ids`: list visible range endpoints
- `show`: render current future context
- `resume`: render an existing session and optionally append a human prompt
- `prompts`: list or print shared prompt fragments

`--text` is one exact CLI string. `--text-file PATH` reads exact text from a file, and `--text-file -` reads stdin. This avoids joining separate argv words, which can alter whitespace.

In this CLI, `input` means the bytes pcodx records for one message. For `turn` and `resume --text...`, that input is a human prompt. For `record`, it can be a system, developer, user, assistant, or tool message.

`rendered context` means the visible transcript text that pcodx prints from its SQLite ledger after applying compactions. Preserved messages are printed verbatim with an appended id marker like `<aboveturn id="msg1"/>`; compacted ranges are printed as summaries with ids like `<aboveturn id="cmp1"/>`.

KV-cache reuse means reusing a model server's cached computation for an unchanged prefix of a conversation. pcodx does not claim native KV-cache reuse across a fresh compacted Codex session. The current claim is narrower: pcodx stores original message text unchanged, appends ids after preserved turns, and applies compaction only when rendering future context from the ledger.

`dynamic tools` means tools registered with a future app-server session at runtime, such as partial-compaction tools the model could call. It does not mean redefining slash commands in this standalone CLI skeleton.

## install

Build/install with:

```sh
git submodule update --init --recursive
cargo install --path . --locked --root ~/.local
```

This installs `pcodx` to `~/.local/bin` if that directory is on `PATH`.

## storage

Default database: `$PCODX_DB`, else `$XDG_DATA_HOME/pcodx/pcodx.sqlite3`, else `~/.local/share/pcodx/pcodx.sqlite3`.

Tables:

- `sessions`: session id, working directory, update time, upstream Codex id placeholder, and `kv_cache_boundary`
- `messages`: full message text, role, stable `msg1` ids, source, and a human-prompt flag
- `compactions`: stable `cmp1` ids, covered message range, summary, and replacement count

Human prompts are stored exactly as supplied. Compaction allows any range, including user prompts that contain bulky logs. If the range includes system, developer, or user messages, `pcodx compact` prints a warning that the summary must preserve active instructions and human intent.

## kv-cache boundary

PCODX should not try to mutate a live hidden Codex transcript. The safe boundary is future-turn rendering: preserved messages are emitted in original order with a minimal marker appended after the turn, while compacted ranges are replaced by summaries. A future app-server integration should seed a fresh upstream thread with this render after compaction.

## prompt source

`vendor/agent_partial_compact_common` is a Git submodule containing the OpenCode partial-compaction prompt fragments verbatim. The test suite checks embedded prompt bytes against that submodule.

## deferred

- real Codex app-server proxy/front-end launch
- dynamic tool registration
- native Codex session id mapping

Old proof-of-concept JSON migration is a non-goal. Those files were evidence for the prototype; the Rust rewrite starts with a clean SQLite history.
