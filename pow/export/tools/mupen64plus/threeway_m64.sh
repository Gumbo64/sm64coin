#!/bin/sh
# Port vs stock Mupen64Plus on ONE movie (a power-on m64: a cycle's, or a v0.4 archived proof's made
# with --record-m64): plays it in Mupen64Plus with one leading zero poll (the core polls once more at
# boot), dumps Mario / RNG / global timer at every poll and compares with the port.
# Exit 2 on any difference.
#   threeway_m64.sh MOVIE.m64 WORKDIR
# Env: SM64COIN_ROM (US ROM), SM64COIN_EXPORT (the sm64coin-export binary). Same rig as threeway.sh.
set -e
M=${1:?movie.m64}; W=${2:?work dir}
H=$(cd "$(dirname "$0")" && pwd); mkdir -p "$W"
EXE=${SM64COIN_EXPORT:-sm64coin-export}
[ -x "$W/frontend" ] && [ "$W/m64_input.c.stamp" -nt "$H/m64_input.c" ] 2>/dev/null || { "$H/build.sh" "$W"; touch "$W/m64_input.c.stamp"; }
"$EXE" lead-poll "$M" "$W/lead.m64"
FRAMES=$("$EXE" m64-frames "$M")
# Mupen keeps the EEPROM between runs, like the console: every run needs a fresh HOME.
rm -rf "${W:?}/home"; mkdir -p "$W/home"
HOME="$W/home" XDG_DATA_HOME="$W/home/.local/share" XDG_CONFIG_HOME="$W/home/.config" M64_FILE="$W/lead.m64" \
  M64_DUMP="1-$((FRAMES + 2))" M64_DUMP_NOPOOL=1 M64_DUMP_FILE="$W/dump.bin" M64_QUIT_AT=$((FRAMES + 20)) \
  "$W/frontend" "$SM64COIN_ROM" /usr/lib/x86_64-linux-gnu/mupen64plus/mupen64plus-rsp-hle.so "$W/m64_input.so" "$W/null_video.so" >"$W/run.log" 2>&1
echo "== port vs Mupen64Plus, $(basename "$M") ($FRAMES frames)"
"$EXE" compare-mupen-dump "$M" "$W/dump.bin"
