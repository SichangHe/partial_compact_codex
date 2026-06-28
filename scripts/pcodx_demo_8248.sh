#!/usr/bin/env bash
set -euo pipefail

DB=/tmp/pcodx-demo-8248.sqlite3
SESSION=pcodx-demo-8248
RENDER=/tmp/pcodx-demo-8248-rendered-after-compact.txt
RESUME=/tmp/pcodx-demo-8248-rendered-after-resume.txt
KEPT=/tmp/pcodx-demo-8248-kept-from-render.txt
KEPT_RESUME=/tmp/pcodx-demo-8248-kept-from-resume.txt

rm -f "$DB" "$RENDER" "$RESUME" "$KEPT" "$KEPT_RESUME"

printf "PCODX inspectable forget/keep/resume demo\n"
printf "repo: %s\n" "$PWD"
printf "db: %s\n" "$DB"
printf "session: %s\n\n" "$SESSION"

printf "1. help now describes the CLI and commands\n"
./target/debug/pcodx --help | sed -n "1,32p"

printf "\n2. start a pcodx session\n"
./target/debug/pcodx --db "$DB" --session "$SESSION" init

printf "\n3. read three long files into the session\n"
./target/debug/pcodx --db "$DB" --session "$SESSION" record --role assistant --text-file README.md --source beginning-readme
./target/debug/pcodx --db "$DB" --session "$SESSION" record --role assistant --text-file src/storage.rs --source middle-storage
./target/debug/pcodx --db "$DB" --session "$SESSION" record --role assistant --text-file Cargo.lock --source ending-cargo-lock
printf "visible ids before forget: "
./target/debug/pcodx --db "$DB" --session "$SESSION" ids | tail -n +2 | paste -sd, -

printf "\n4. forget beginning and ending files; keep middle file msg2 intact\n"
./target/debug/pcodx --db "$DB" --session "$SESSION" compact --from msg1 --to msg1 --summary "FORGOTTEN beginning file README.md: intentionally unavailable after compaction."
./target/debug/pcodx --db "$DB" --session "$SESSION" compact --from msg3 --to msg3 --summary "FORGOTTEN ending file Cargo.lock: intentionally unavailable after compaction."
./target/debug/pcodx --db "$DB" --session "$SESSION" show > "$RENDER"
printf "rendered context saved: %s\n" "$RENDER"
printf "visible ids after forget: "
./target/debug/pcodx --db "$DB" --session "$SESSION" ids | tail -n +2 | paste -sd, -

python3 - "$DB" <<'PY'
import sqlite3
import sys
db_path = sys.argv[1]
with sqlite3.connect(db_path) as conn:
    kept = conn.execute(
        "SELECT text FROM messages WHERE session_id = ? AND id = ?",
        ("pcodx-demo-8248", "msg2"),
    ).fetchone()[0]
with open("src/storage.rs", encoding="utf-8") as handle:
    expected = handle.read()
if kept != expected:
    raise SystemExit("stored msg2 differs from src/storage.rs")
PY
rg -q '<aboveturn id="msg2"/>' "$RENDER"
printf "PASS: kept middle file src/storage.rs is stored verbatim and visible after compaction\n"

if rg -q "The proof-of-concept stored JSON ledgers" "$RENDER"; then
  printf "FAIL: forgotten README content still present\n"
  exit 1
fi
printf "PASS: forgotten beginning README exact content is absent\n"

if rg -q 'source = "registry\+https://github.com/rust-lang/crates.io-index"' "$RENDER"; then
  printf "FAIL: forgotten Cargo.lock content still present\n"
  exit 1
fi
printf "PASS: forgotten ending Cargo.lock exact content is absent\n"

printf "\n5. exit happened because each pcodx command returned; now resume the same wrapper session\n"
./target/debug/pcodx --db "$DB" --session "$SESSION" resume > "$RESUME"
printf "resume render saved: %s\n" "$RESUME"

python3 - "$DB" <<'PY'
import sqlite3
import sys
db_path = sys.argv[1]
with sqlite3.connect(db_path) as conn:
    kept = conn.execute(
        "SELECT text FROM messages WHERE session_id = ? AND id = ?",
        ("pcodx-demo-8248", "msg2"),
    ).fetchone()[0]
with open("src/storage.rs", encoding="utf-8") as handle:
    expected = handle.read()
if kept != expected:
    raise SystemExit("stored msg2 differs from src/storage.rs after resume")
PY
rg -q '<aboveturn id="msg2"/>' "$RESUME"
printf "PASS: kept middle file src/storage.rs is stored verbatim and visible after resume\n"

if rg -q "The proof-of-concept stored JSON ledgers" "$RESUME"; then
  printf "FAIL: forgotten README content returned after resume\n"
  exit 1
fi
printf "PASS: forgotten beginning README exact content remains absent after resume\n"

if rg -q 'source = "registry\+https://github.com/rust-lang/crates.io-index"' "$RESUME"; then
  printf "FAIL: forgotten Cargo.lock content returned after resume\n"
  exit 1
fi
printf "PASS: forgotten ending Cargo.lock exact content remains absent after resume\n"

printf "\nDemo complete. Inspect this pane, or inspect artifacts:\n"
printf "  %s\n" "$DB"
printf "  %s\n" "$RENDER"
printf "  %s\n" "$RESUME"
