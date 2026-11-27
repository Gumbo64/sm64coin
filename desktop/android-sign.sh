#!/usr/bin/env bash
# Align and sign the release APK, verify the signature, and refuse a certificate that is not SM64Coin's release key.
# usage: android-sign.sh <unsigned.apk> <signed.apk>
#   ANDROID_KEYSTORE            path of the .jks (alias sm64coin)
#   ANDROID_KEYSTORE_PASSWORD   its password (or ANDROID_KEYSTORE_PASSWORD_FILE: a file holding it)
#   ANDROID_HOME                the SDK (build-tools: zipalign, apksigner)
# The password is handed to apksigner through the environment, never on a command line, and never printed.
set -euo pipefail
IN="${1:?usage: android-sign.sh <unsigned.apk> <signed.apk>}"; OUT="${2:?usage: android-sign.sh <unsigned.apk> <signed.apk>}"
EXPECTED="63:10:2D:9D:EE:E7:3F:03:D2:CF:EB:6A:61:E6:77:B1:4E:2A:4B:92:55:60:D2:8B:D5:53:ED:42:4E:45:9A:C4"
: "${ANDROID_KEYSTORE:?ANDROID_KEYSTORE is not set}"
if [ -z "${ANDROID_KEYSTORE_PASSWORD:-}" ] && [ -n "${ANDROID_KEYSTORE_PASSWORD_FILE:-}" ]; then
  ANDROID_KEYSTORE_PASSWORD="$(tr -d '\r\n' < "$ANDROID_KEYSTORE_PASSWORD_FILE")"
fi
: "${ANDROID_KEYSTORE_PASSWORD:?no keystore password}"
# A password pasted from a phone can carry invisible non-ASCII characters (no-break space, zero-width space, BOM, smart quotes) and
# keytool rejects those ("Password is not ASCII"). Keep only printable ASCII; say how many characters were dropped, never which.
clean="$(printf '%s' "$ANDROID_KEYSTORE_PASSWORD" | LC_ALL=C tr -cd '\041-\176')"
if [ "${#clean}" != "$(printf '%s' "$ANDROID_KEYSTORE_PASSWORD" | LC_ALL=C wc -c | tr -d ' ')" ]; then
  echo "android-sign: dropped $(( $(printf '%s' "$ANDROID_KEYSTORE_PASSWORD" | LC_ALL=C wc -c) - ${#clean} )) non-printable/non-ASCII byte(s) from the keystore password" >&2
fi
ANDROID_KEYSTORE_PASSWORD="$clean"
export ANDROID_KEYSTORE_PASSWORD
BT="$(ls -d "${ANDROID_HOME:?ANDROID_HOME is not set}"/build-tools/*/ | sort -V | tail -1)"
tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' EXIT
"$BT/zipalign" -f -p 4 "$IN" "$tmp/aligned.apk"
"$BT/apksigner" sign --ks "$ANDROID_KEYSTORE" --ks-key-alias sm64coin --ks-pass env:ANDROID_KEYSTORE_PASSWORD \
  --key-pass env:ANDROID_KEYSTORE_PASSWORD --out "$OUT" "$tmp/aligned.apk"
report="$("$BT/apksigner" verify --verbose --print-certs "$OUT")"
printf '%s\n' "$report"
# The line is "Signer #1 certificate SHA-256 digest: <hex>" in older apksigner and "V3.0 Signer: certificate SHA-256 digest: <hex>" in newer.
got="$(printf '%s\n' "$report" | grep -m1 -oE 'certificate SHA-256 digest: [0-9a-fA-F]+' | awk '{print $NF}' | tr 'a-f' 'A-F' | sed 's/../&:/g; s/:$//')"
echo "signing certificate SHA-256: $got"
if [ "$got" != "$EXPECTED" ]; then echo "android-sign: certificate differs from the SM64Coin release key ($EXPECTED)" >&2; rm -f "$OUT"; exit 1; fi
