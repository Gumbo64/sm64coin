#!/bin/sh
# Replays one cycle of an export in stock Mupen64Plus (headless) and compares Mario's state at every
# block boundary (and every 5000 frames) with the port's. Needs the export made with --mupen64plus.
#   check_cycle.sh EXPORT_DIR CYCLE WORKDIR
# Env: SM64COIN_ROM (the US ROM), SM64COIN_EXPORT (the sm64coin-export binary).
set -e
EXP=${1:?export dir}; N=$(printf %04d "${2:?cycle}"); W=${3:?work dir}
H=$(cd "$(dirname "$0")" && pwd); mkdir -p "$W/st"; rm -f "$W"/st/*.st
[ -x "$W/frontend" ] || "$H/build.sh" "$W"
POLLS=$(python3 - "$EXP/cycles/cycle_$N.json" <<'P'
import json, sys
c = json.load(open(sys.argv[1]))
fr = sorted({b["end_frame"] for b in c["blocks"]} | set(range(5000, c["frames"], 5000)))
print(",".join(str(f + 2) for f in fr))   # lead poll + f frames done, requested in the next poll
P
)
LAST=$(echo "$POLLS" | tr ',' '\n' | tail -1)
# Mupen64Plus keeps the game's save file (EEPROM) between runs, like the console; the port starts blank.
rm -rf "$W/home"; mkdir -p "$W/home"
HOME="$W/home" XDG_DATA_HOME="$W/home/.local/share" XDG_CONFIG_HOME="$W/home/.config" M64_FILE="$EXP/cycles/cycle_$N.mupen64plus.m64" M64_SAVE_AT="$POLLS" M64_SAVE_DIR="$W/st" M64_QUIT_AT=$((LAST + 200)) \
  "$W/frontend" "$SM64COIN_ROM" /usr/lib/x86_64-linux-gnu/mupen64plus/mupen64plus-rsp-hle.so "$W/m64_input.so" "$W/null_video.so" >"$W/run.log" 2>&1
"${SM64COIN_EXPORT:-sm64coin-export}" compare-mupen "$EXP/cycles/cycle_$N.m64" 1 "$W"/st/after_*.st
