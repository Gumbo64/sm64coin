#!/bin/sh
# Three-way check of one exported cycle: the port (the game build the chain runs), Mupen64Plus (the N64
# build running the ROM) and, if SM64_PHYSICS_DIR and ORACLE are set, sm64_physics (a native build of the
# decomp that is checked against Mupen) through sm64-binds' oracle tool.
#   threeway.sh EXPORT_DIR CYCLE WORKDIR
# Needs an export made with --mupen64plus. Env: SM64COIN_ROM, SM64COIN_EXPORT (the binary);
# optional ORACLE (sm64-binds/oracle's `oracle` binary) and SM64_PHYSICS_DIR (its checkout, for the oracle's
# build only; nothing of it is copied anywhere).
# Compares at EVERY controller poll: Mario (action, position, velocity, health, stars), the RNG word and the
# global timer, port vs Mupen64Plus (poll-level dump through the core's debugger memory pointer, no save
# states: the core completes only the first few state saves of a run). Exit 2 on any difference.
set -e
EXP=${1:?export dir}; N=$(printf %04d "${2:?cycle}"); W=${3:?work dir}
H=$(cd "$(dirname "$0")" && pwd); mkdir -p "$W"
[ -x "$W/frontend" ] && [ "$W/m64_input.c.stamp" -nt "$H/m64_input.c" ] 2>/dev/null || { "$H/build.sh" "$W"; touch "$W/m64_input.c.stamp"; }
M64="$EXP/cycles/cycle_$N.mupen64plus.m64"
FRAMES=$(python3 -c "import json,sys;print(json.load(open(sys.argv[1]))['frames'])" "$EXP/cycles/cycle_$N.json")
rm -rf "$W/home"; mkdir -p "$W/home"   # Mupen keeps the EEPROM between runs, like the console
HOME="$W/home" XDG_DATA_HOME="$W/home/.local/share" XDG_CONFIG_HOME="$W/home/.config" M64_FILE="$M64" \
  M64_DUMP="1-$((FRAMES + 2))" M64_DUMP_NOPOOL=1 M64_DUMP_FILE="$W/dump_$N.bin" M64_QUIT_AT=$((FRAMES + 20)) \
  "$W/frontend" "$SM64COIN_ROM" /usr/lib/x86_64-linux-gnu/mupen64plus/mupen64plus-rsp-hle.so "$W/m64_input.so" "$W/null_video.so" >"$W/run_$N.log" 2>&1
STATUS=0
echo "== port vs Mupen64Plus, cycle $N ($FRAMES frames)"
"${SM64COIN_EXPORT:-sm64coin-export}" compare-mupen-dump "$EXP/cycles/cycle_$N.m64" "$W/dump_$N.bin" || STATUS=2
if [ -n "$ORACLE" ] && [ -n "$SM64_PHYSICS_DIR" ]; then
  echo "== port vs sm64_physics (oracle), cycle $N"
  "$ORACLE" "$EXP/cycles/cycle_$N.m64" | tail -3 | cut -c1-300
  "$ORACLE" "$EXP/cycles/cycle_$N.m64" | grep -q "identical for all" || STATUS=2
fi
exit $STATUS
