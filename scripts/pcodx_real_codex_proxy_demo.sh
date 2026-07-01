#!/usr/bin/env bash
set -euo pipefail

TARGET=${PCODX_REAL_DEMO_TMUX:-pcodx-real-codex-proxy-demo}
LISTEN=${PCODX_REAL_DEMO_LISTEN:-ws://127.0.0.1:48570}
UPSTREAM=${PCODX_REAL_DEMO_UPSTREAM:-ws://127.0.0.1:48571}
DB=${PCODX_REAL_DEMO_DB:-/tmp/pcodx-real-codex-proxy-demo.sqlite3}
SESSION=${PCODX_REAL_DEMO_SESSION:-pcodx-real-codex-proxy-demo}
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)

shell_quote() {
  printf "%q" "$1"
}

wait_tcp() {
  local url=$1
  python3 - "$url" <<'PY'
import socket
import sys
import time

url = sys.argv[1]
if not url.startswith("ws://"):
    raise SystemExit(f"demo wait only supports ws:// URLs: {url}")
host, port_text = url.removeprefix("ws://").split(":", 1)
deadline = time.monotonic() + 10
while time.monotonic() < deadline:
    try:
        with socket.create_connection((host, int(port_text)), timeout=0.2):
            raise SystemExit(0)
    except OSError:
        time.sleep(0.1)
raise SystemExit(f"timed out waiting for {url}")
PY
}

cd "$ROOT"
cargo build --locked >/tmp/pcodx-real-codex-proxy-demo-build.out
rm -f "$DB"
./target/debug/pcodx --db "$DB" --session "$SESSION" init >/tmp/pcodx-real-codex-proxy-demo-init.out
./target/debug/pcodx --db "$DB" --session "$SESSION" record --role assistant --text "seeded proxy demo setup; safe to compact" --source real-proxy-demo >/tmp/pcodx-real-codex-proxy-demo-msg1.out
./target/debug/pcodx --db "$DB" --session "$SESSION" record --role assistant --text "seeded proxy demo durable fact; keep visible" --source real-proxy-demo >/tmp/pcodx-real-codex-proxy-demo-msg2.out
if tmux has-session -t "$TARGET" 2>/dev/null; then
  tmux kill-session -t "$TARGET"
fi
PROXY_CMD="./target/debug/pcodx --db $(shell_quote "$DB") --session $(shell_quote "$SESSION") serve --listen $(shell_quote "$LISTEN") --upstream $(shell_quote "$UPSTREAM") --enable-pcodx-tools"
FRONTEND_CMD=$(
  printf '%s\n' \
    "cd $(shell_quote "$ROOT")" \
    "printf '%s\n' 'real Codex frontend -> pcodx middleware -> real Codex app-server'" \
    "printf '%s\n' 'PCODX dynamic tools are bound to seeded demo session: $(shell_quote "$SESSION")'" \
    "printf '%s\n' 'type a prompt, then /exit; after exit this pane runs codex resume --last through the same middleware'" \
    "codex --remote $(shell_quote "$LISTEN") --no-alt-screen -C $(shell_quote "$ROOT")" \
    "printf '%s\n' 'front end exited; resuming through pcodx middleware'" \
    "codex resume --last --remote $(shell_quote "$LISTEN") --no-alt-screen -C $(shell_quote "$ROOT")" \
    "exec \"\${SHELL:-/bin/sh}\" -i"
)
tmux new-session -d -s "$TARGET" -c "$ROOT" "$PROXY_CMD"
wait_tcp "$LISTEN"
tmux split-window -h -t "$TARGET" -c "$ROOT" "$FRONTEND_CMD"
tmux select-pane -t "$TARGET":0.1
printf "started tmux demo: %s\n" "$TARGET"
printf "attach with: tmux attach -t %s\n" "$TARGET"
