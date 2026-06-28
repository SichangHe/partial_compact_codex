# pcodx

Codex, but with partial compaction.

The product shape is Codex on both sides: Codex TUI is the frontend, Codex app-server/model path is the backend, and `pcodx` sits between them to do partial compaction. The wrapper should preserve KV-cache-compatible context as much as possible.

The wrapper changes context only in two cases:

- append a minimal turn id after a completed turn
- replace a compacted range with the summary supplied by the agent

The current Rust prototype implements the durable context/rendering core and an inspectable Codex-like terminal demo. It does not yet proxy the real Codex app-server, so it is not the final frontend/backend wrapper.

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

`resume` renders stored compacted context, so it is not an empty session. The storage detail is an implementation detail of the prototype, not the product framing.

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

In this CLI, `input` means the bytes pcodx records for one turn. For `turn` and `resume --text...`, that input is a human prompt. For `record`, it can be a system, developer, user, assistant, or tool message.

`rendered context` means the future Codex context after applying compactions. Preserved turns are printed verbatim with an appended id marker like `<aboveturn id="msg1"/>`; compacted ranges are printed as summaries with ids like `<aboveturn id="cmp1"/>`.

KV-cache reuse means reusing a model server's cached computation for an unchanged prefix of a conversation. The intended wrapper keeps that prefix stable except for the two required mutations above. The current prototype stores original turn text unchanged, appends ids only in rendered context, and replaces only compacted ranges with summaries.

`dynamic tools` means tools registered with a future app-server session at runtime, such as partial-compaction tools the model could call. It does not mean redefining slash commands in this CLI prototype.

## demo

Run the Codex-like partial-compaction demo in tmux:

```sh
scripts/pcodx_codex_like_demo.sh
tmux attach -t pcodx-codex-like-demo
```

The pane opens a Codex-like terminal, reads three files, compacts the beginning and ending turns, keeps the middle turn visible, exits, resumes, and repeats the checks after resume.

## install

Build/install with:

```sh
git submodule update --init --recursive
cargo install --path . --locked --root ~/.local
```

This installs `pcodx` to `~/.local/bin` if that directory is on `PATH`.

## storage

Default prototype database: `$PCODX_DB`, else `$XDG_DATA_HOME/pcodx/pcodx.sqlite3`, else `~/.local/share/pcodx/pcodx.sqlite3`.

Tables:

- `sessions`: session id, working directory, update time, upstream Codex id placeholder, and `kv_cache_boundary`
- `messages`: full message text, role, stable `msg1` ids, source, and a human-prompt flag
- `compactions`: stable `cmp1` ids, covered message range, summary, and replacement count

Human prompts are stored exactly as supplied. Compaction allows any range, including user prompts that contain bulky logs. If the range includes system, developer, or user messages, `pcodx compact` prints a warning that the summary must preserve active instructions and human intent.

## kv-cache boundary

The intended app-server wrapper should not change Codex context except by appending ids after completed turns and replacing compacted ranges with summaries. The current prototype demonstrates that render boundary, but does not yet preserve native Codex KV cache across a real compacted app-server session.

## prompt source

`vendor/agent_partial_compact_common` is a Git submodule containing shared partial-compaction prompt fragments. The test suite checks embedded prompt bytes against that submodule.

## deferred

- real Codex app-server proxy between Codex frontend and Codex backend
- dynamic tool registration
- native Codex session id mapping

Historical JSON migration is a non-goal. Those files were evidence for earlier experiments; this prototype starts with a clean durable history.
