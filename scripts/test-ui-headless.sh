#!/usr/bin/env bash
# Run GTK4/Libadwaita widget, window, row, and dialog E2E tests headlessly
# inside an isolated gtk4-broadwayd instance (no Wayland/X11 desktop needed).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

if ! command -v gtk4-broadwayd >/dev/null 2>&1; then
  echo "error: gtk4-broadwayd not found (install gtk4 on Fedora)" >&2
  exit 1
fi

TMP_XDG="$(mktemp -d)"
BPID=""
cleanup() {
  if [[ -n "$BPID" ]]; then
    kill "$BPID" 2>/dev/null || true
    wait "$BPID" 2>/dev/null || true
  fi
  rm -rf "$TMP_XDG"
}
trap cleanup EXIT

PORT=""
if command -v python3 >/dev/null 2>&1; then
  PORT="$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1]); s.close()')"
fi
if [[ -z "$PORT" ]]; then
  PORT=$((18000 + ($$ % 10000)))
fi

DISP=":$((40 + ($$ % 40)))"
XDG_RUNTIME_DIR="$TMP_XDG" gtk4-broadwayd --address=127.0.0.1 --port="$PORT" "$DISP" >"$TMP_XDG/broadway.log" 2>&1 &
BPID=$!

READY=0
for _ in $(seq 1 60); do
  if compgen -G "$TMP_XDG/broadway*.socket" >/dev/null 2>&1; then
    READY=1
    break
  fi
  if ! kill -0 "$BPID" 2>/dev/null; then
    break
  fi
  sleep 0.05
done

if [[ "$READY" -ne 1 ]]; then
  echo "error: gtk4-broadwayd failed to start on $DISP (port $PORT)" >&2
  cat "$TMP_XDG/broadway.log" >&2 || true
  exit 1
fi

exec env \
  -u DISPLAY \
  -u WAYLAND_DISPLAY \
  XDG_RUNTIME_DIR="$TMP_XDG" \
  GDK_BACKEND=broadway \
  BROADWAY_DISPLAY="$DISP" \
  FEDORA_UPDATER_REQUIRE_GTK=1 \
  cargo test --bin fedora-updater -- --test-threads=1 "$@"
