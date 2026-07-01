#!/usr/bin/env bash
set -euo pipefail

TARGET=${PCODX_DEMO_TMUX:-pcodx-codex-like-demo}
DB=${PCODX_DEMO_DB:-/tmp/pcodx-codex-like-demo.sqlite3}
SESSION=${PCODX_DEMO_SESSION:-pcodx-codex-like-demo}
RENDER=${PCODX_DEMO_RENDER:-/tmp/pcodx-codex-like-demo-render.txt}
RESUME=${PCODX_DEMO_RESUME:-/tmp/pcodx-codex-like-demo-resume.txt}
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)

say() {
  printf "%s\n" "$*"
  sleep 0.25
}

prompt() {
  printf "\n> %s\n" "$*"
  sleep 0.5
}

pass() {
  printf "  ok  %s\n" "$*"
}

shell_quote() {
  printf "%q" "$1"
}

check_absent() {
  local needle=$1
  local file=$2
  local label=$3
  if rg -q "$needle" "$file"; then
    printf "  fail  %s\n" "$label"
    exit 1
  fi
  pass "$label"
}

check_kept_verbatim() {
  local label=$1
  python3 - "$DB" "$SESSION" "$ROOT/src/storage.rs" <<'PY'
import sqlite3
import sys

db_path, session_id, expected_path = sys.argv[1:]
with sqlite3.connect(db_path) as conn:
    stored = conn.execute(
        "SELECT text FROM messages WHERE session_id = ? AND id = 'msg2'",
        (session_id,),
    ).fetchone()[0]
with open(expected_path, encoding="utf-8") as handle:
    expected = handle.read()
if stored != expected:
    raise SystemExit("msg2 is not stored verbatim")
PY
  pass "$label"
}

check_rendered_kept_verbatim() {
  local file=$1
  local label=$2
  python3 - "$file" "$ROOT/src/storage.rs" <<'PY'
import sys

render_path, expected_path = sys.argv[1:]
with open(render_path, encoding="utf-8") as handle:
    rendered = handle.read()
with open(expected_path, encoding="utf-8") as handle:
    expected = handle.read()
if f"{expected}\n<aboveturn id=\"msg2\"/>" not in rendered:
    raise SystemExit("rendered msg2 is not byte-for-byte followed by its id marker")
PY
  pass "$label"
}

check_rendered_original_absent() {
  local file=$1
  local message_id=$2
  local label=$3
  python3 - "$DB" "$SESSION" "$file" "$message_id" <<'PY'
import sqlite3
import sys

db_path, session_id, render_path, message_id = sys.argv[1:]
with sqlite3.connect(db_path) as conn:
    original = conn.execute(
        "SELECT text FROM messages WHERE session_id = ? AND id = ?",
        (session_id, message_id),
    ).fetchone()[0]
with open(render_path, encoding="utf-8") as handle:
    rendered = handle.read()
if original in rendered:
    raise SystemExit(f"original {message_id} text is still rendered")
if f'<aboveturn id="{message_id}"/>' in rendered:
    raise SystemExit(f"original {message_id} marker is still rendered")
PY
  pass "$label"
}

check_compaction_marker() {
  local file=$1
  local summary=$2
  local marker=$3
  local label=$4
  python3 - "$file" "$summary" "$marker" <<'PY'
import sys

render_path, summary, marker = sys.argv[1:]
with open(render_path, encoding="utf-8") as handle:
    rendered = handle.read()
if f"{summary}\n{marker}" not in rendered:
    raise SystemExit("compaction summary or marker is missing")
PY
  pass "$label"
}

run_inner() {
  cd "$ROOT"
  rm -f "$DB" "$RENDER" "$RESUME"
  clear
  say "pcodx interactive"
  say "workdir: $ROOT"
  say "mode: current Codex-like PCODX frontend"
  say ""
  say "This pane is pcodx interactive, the current Codex-like frontend."
  say "The real Codex proxy exists, but it cannot yet route the next native turn through a fresh compacted upstream thread."
  say "This demo proves rendered future-context forgetting/retention, not model recall."
  say ""
  ./target/debug/pcodx --db "$DB" --session "$SESSION" init >/tmp/pcodx-codex-like-demo-init.out
  prompt "pcodx interactive reads README.md, src/storage.rs, and Cargo.lock"
  printf '%s\n' \
    "/record-file assistant README.md" \
    "/record-file assistant src/storage.rs" \
    "/record-file assistant Cargo.lock" \
    "/ids" \
    "/exit" \
    | ./target/debug/pcodx --db "$DB" --session "$SESSION" interactive
  say "visible turns before compaction: $(./target/debug/pcodx --db "$DB" --session "$SESSION" ids | tail -n +2 | paste -sd, -)"
  prompt "pcodx interactive partially compacts msg1 and msg3; msg2 remains retained"
  printf '%s\n' \
    "/compact msg1..msg1 FORGOTTEN beginning read: README.md was intentionally compacted." \
    "/compact msg3..msg3 FORGOTTEN ending read: Cargo.lock was intentionally compacted." \
    "/turn future query: recite exact details from forgotten README.md and Cargo.lock, then from retained src/storage.rs" \
    "/show" \
    "/exit" \
    | ./target/debug/pcodx --db "$DB" --session "$SESSION" interactive
  ./target/debug/pcodx --db "$DB" --session "$SESSION" show >"$RENDER"
  say "visible turns after compaction: $(./target/debug/pcodx --db "$DB" --session "$SESSION" ids | tail -n +2 | paste -sd, -)"
  say "checks before exit:"
  rg -q '<aboveturn id="msg2"/>' "$RENDER"
  pass "kept middle turn is visible as msg2"
  check_kept_verbatim "kept middle file is stored verbatim"
  check_rendered_kept_verbatim "$RENDER" "kept middle file renders verbatim"
  check_compaction_marker "$RENDER" "FORGOTTEN beginning read: README.md was intentionally compacted." '<aboveturn id="cmp1"/>' "beginning compaction summary is visible as cmp1"
  check_compaction_marker "$RENDER" "FORGOTTEN ending read: Cargo.lock was intentionally compacted." '<aboveturn id="cmp2"/>' "ending compaction summary is visible as cmp2"
  check_rendered_original_absent "$RENDER" msg1 "compacted README original message and marker are absent"
  check_rendered_original_absent "$RENDER" msg3 "compacted Cargo.lock original message and marker are absent"
  check_absent "Codex, but with partial compaction" "$RENDER" "README representative phrase is absent"
  check_absent 'source = "registry\+https://github.com/rust-lang/crates.io-index"' "$RENDER" "Cargo.lock representative phrase is absent"
  prompt "/exit"
  say "session closed"
  prompt "pcodx resume --last, then ask the same forgotten-vs-retained question"
  ./target/debug/pcodx --db "$DB" resume --last --text "after resume future query: recite exact details from forgotten README.md and Cargo.lock, then from retained src/storage.rs" >"$RESUME" 2>/tmp/pcodx-codex-like-demo-resume-meta.out
  say "checks after resume:"
  rg -q '<aboveturn id="msg2"/>' "$RESUME"
  pass "kept middle turn is visible after resume"
  check_kept_verbatim "kept middle file is still stored verbatim"
  check_rendered_kept_verbatim "$RESUME" "kept middle file still renders verbatim"
  check_compaction_marker "$RESUME" "FORGOTTEN beginning read: README.md was intentionally compacted." '<aboveturn id="cmp1"/>' "beginning compaction summary remains visible as cmp1"
  check_compaction_marker "$RESUME" "FORGOTTEN ending read: Cargo.lock was intentionally compacted." '<aboveturn id="cmp2"/>' "ending compaction summary remains visible as cmp2"
  check_rendered_original_absent "$RESUME" msg1 "compacted README original message and marker stay absent"
  check_rendered_original_absent "$RESUME" msg3 "compacted Cargo.lock original message and marker stay absent"
  check_absent "Codex, but with partial compaction" "$RESUME" "README representative phrase stays absent"
  check_absent 'source = "registry\+https://github.com/rust-lang/crates.io-index"' "$RESUME" "Cargo.lock representative phrase stays absent"
  say ""
  say "demo complete"
  say "render before resume: $RENDER"
  say "render after resume:  $RESUME"
  say "database:             $DB"
  say ""
  say "The shell is left open so the pane is inspectable."
  exec "${SHELL:-/bin/sh}" -i
}

if [[ "${PCODX_DEMO_INNER:-}" == "1" ]]; then
  run_inner
fi

cd "$ROOT"
cargo build --locked >/tmp/pcodx-codex-like-demo-build.out
if tmux has-session -t "$TARGET" 2>/dev/null; then
  tmux kill-session -t "$TARGET"
fi
INNER_CMD="PCODX_DEMO_INNER=1 PCODX_DEMO_DB=$(shell_quote "$DB") PCODX_DEMO_SESSION=$(shell_quote "$SESSION") PCODX_DEMO_RENDER=$(shell_quote "$RENDER") PCODX_DEMO_RESUME=$(shell_quote "$RESUME") $(shell_quote "$0")"
tmux new-session -d -s "$TARGET" -c "$ROOT" "$INNER_CMD"
printf "started tmux demo: %s\n" "$TARGET"
printf "attach with: tmux attach -t %s\n" "$TARGET"
