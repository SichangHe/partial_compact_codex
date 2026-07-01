pcodx demo requirement

- required human-review demo
  - provide a tmux window running the current PCODX user-facing frontend
  - read several files through that frontend
  - partially compact selected file reads while retaining at least one other file read
  - ask for details from forgotten and retained file reads after compaction
  - show forgotten file details are absent from rendered future context
  - show retained file details remain visible verbatim
  - exit the session
  - resume the same session
  - ask the same forgotten-vs-retained question after resume
  - show the forgotten content remains absent after resume
  - show the retained content remains visible verbatim after resume

- current honest frontend routing
  - `pcodx serve` is the native Codex frontend proxy path
  - `pcodx serve --seed-pcodx-context` appends rendered PCODX context to a native Codex thread
  - it does not replace arbitrary active native Codex history
  - `pcodx interactive` is the current Codex-like frontend that can demonstrate selective forgetting and resume with real PCODX storage and rendering
  - it does not produce model answers
  - the current `pcodx interactive` tmux demo proves rendered future-context behavior, not model recall

- exact remaining blocker
  - full human-requested agent-recitation proof needs either native Codex history ingestion plus live active-context replacement, or another real answering frontend whose model context is PCODX-rendered context after each compaction and resume

- runnable review command
  - `scripts/pcodx_codex_like_demo.sh`
  - attach with `tmux attach -t pcodx-codex-like-demo`
