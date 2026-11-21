#!/usr/bin/env bash
# The SM64Coin desktop wallet (gui/) in a browser tab, for machines without a
# display (containers, servers): a virtual X display (Xvfb) served by x11vnc,
# with noVNC in front. Then open
#   http://localhost:${GUI_WEB_PORT:-6080}/vnc.html?autoconnect=1&resize=scale
# On a desktop, just run bin/sm64coin-wallet-gui directly.
# Env: GUI_DISPLAY (:77), GUI_VNC_PORT (5901), GUI_WEB_PORT (6080), GUI_SIZE (1280x800).
# Needs: apt install xvfb x11vnc novnc websockify
set -euo pipefail
. "$(dirname "${BASH_SOURCE[0]}")/env.sh"
DISP="${GUI_DISPLAY:-:77}"; VNC="${GUI_VNC_PORT:-5901}"; WEB="${GUI_WEB_PORT:-6080}"; SIZE="${GUI_SIZE:-1280x800}"
BIN="$ROOT/bin/sm64coin-wallet-gui"
[ -x "$BIN" ] || { echo "build the GUI first (gui/SM64COIN.md)" >&2; exit 1; }
up() { [ -f "$RUN_DIR/$1.pid" ] && kill -0 "$(cat "$RUN_DIR/$1.pid")" 2>/dev/null; }
start() { # name, command...
  local name=$1; shift
  if up "$name"; then echo "$name already running (pid $(cat "$RUN_DIR/$name.pid"))"; return; fi
  setsid "$@" >"$RUN_DIR/$name.log" 2>&1 < /dev/null &
  echo $! > "$RUN_DIR/$name.pid"
}
# 24-bit TrueColor; the GUI's colours come out right (Qt's own VNC plugin
# swapped red and blue with noVNC).
start gui-xvfb Xvfb "$DISP" -screen 0 "${SIZE}x24" -nolisten tcp
sleep 1
start gui-wallet env DISPLAY="$DISP" QT_QPA_PLATFORM=xcb "$BIN"
start gui-x11vnc x11vnc -display "$DISP" -localhost -rfbport "$VNC" -forever -shared -nopw -quiet
start gui-novnc websockify --web /usr/share/novnc "$WEB" "127.0.0.1:$VNC"
echo "SM64Coin Wallet: http://localhost:$WEB/vnc.html?autoconnect=1&resize=scale"
echo "(in the wizard choose a remote node: 127.0.0.1:$RPC_PORT)"
