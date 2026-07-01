pcodx demo requirement

- required human-review demo
  - provide a tmux window running the current PCODX user-facing frontend
  - read several files through that frontend
  - partially compact selected file reads while retaining at least one other file read
  - ask a real answering frontend/model for details from forgotten and retained file reads after compaction
  - show the live answer cannot recite forgotten exact content
  - show the live answer can recite retained exact content
  - show rendered context as supporting evidence only
    - forgotten file details are absent from rendered future context
    - retained file details remain visible verbatim
  - exit the session
  - resume the same session
  - ask the same forgotten-vs-retained question to the real answering frontend/model after resume
  - show the live answer still cannot recite forgotten exact content
  - show the live answer still can recite retained exact content
  - show rendered context as supporting evidence only after resume

- current honest frontend routing
  - `pcodx serve` is the native Codex frontend proxy path
  - `pcodx serve --seed-pcodx-context` appends rendered PCODX context to a native Codex thread
  - when the rendered PCODX context changes, a later native start/resume/fork lifecycle response can append the changed render
  - older injected renders remain present in native Codex history
  - it does not replace arbitrary active native Codex history
  - Codex CLI 0.142.4 app-server schema exposes `thread/inject_items`
    - description says it appends raw Responses API items to model-visible history
  - Codex CLI 0.142.4 app-server schema exposes `thread/compact/start`
    - params contain only `threadId`
    - no selected ranges or replacement PCODX context can be supplied
  - Codex CLI 0.142.4 app-server schema exposes `thread/rollback`
    - it drops turns only from the end of a thread
    - it cannot replace an arbitrary middle range
  - `pcodx interactive` is the current Codex-like frontend that can demonstrate selective forgetting and resume with real PCODX storage and rendering
  - it does not produce model answers
  - the current `pcodx interactive` tmux demo proves rendered future-context behavior, not model recall
  - `pcodx serve --seed-pcodx-context` proves changed append-only native seeding, not live middle-range replacement

- exact remaining blocker
  - missing native app-server operation
    - replace selected active model-visible thread history with supplied PCODX-rendered context
  - full human-requested agent-recitation proof needs native Codex history ingestion plus that live active-context replacement
  - OpenCode proof-of-concept does not satisfy the full native-live requirement unless its answering model receives PCODX-rendered context after each compaction and resume

- fallback rendered-context review command
  - no runnable full native-live answering demo exists yet
  - `scripts/pcodx_codex_like_demo.sh`
  - attach with `tmux attach -t pcodx-codex-like-demo`
