#!/bin/sh
# Builds the Mupen64Plus test rig into $1 (needs libmupen64plus-dev, libmupen64plus2, mupen64plus-rsp-hle).
set -e
D=${1:?output dir}; H=$(dirname "$0"); mkdir -p "$D"
gcc -O1 -fPIC -shared -I/usr/include/mupen64plus -o "$D/m64_input.so" "$H/m64_input.c"
gcc -O1 -fPIC -shared -I/usr/include/mupen64plus -o "$D/null_video.so" "$H/null_video.c"
gcc -O1 -I/usr/include/mupen64plus -o "$D/frontend" "$H/frontend.c" -ldl
