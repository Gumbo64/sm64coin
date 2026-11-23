#!/usr/bin/env bash
# Keeps a Cloudflare DNS-only A record (the p2p seed, e.g. p2p.eboy.au) on this machine's public
# IPv4. Run it every few minutes (cron or the systemd timer below) on the seed node.
#   scripts/cf-ddns.sh p2p.eboy.au
# Token: a Cloudflare API token with Zone -> DNS -> Edit for that zone only, in
# ~/.config/cloudflare/token (chmod 600) or $CF_TOKEN. Only changes the record when the IP did.
#
# cron:   */5 * * * * /path/to/scripts/cf-ddns.sh p2p.eboy.au >> ~/cf-ddns.log 2>&1
set -euo pipefail
NAME="${1:?usage: cf-ddns.sh <record name, e.g. p2p.eboy.au>}"
TOKEN="${CF_TOKEN:-$(cat "$HOME/.config/cloudflare/token")}"
API=https://api.cloudflare.com/client/v4
auth=(-H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json")
json() { python3 -c "import json,sys; d=json.load(sys.stdin); print($1)"; }

IP=$(curl -fsS -m 15 https://api.ipify.org || curl -fsS -m 15 https://ipv4.icanhazip.com)
IP=${IP//[[:space:]]/}
[[ "$IP" =~ ^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo "$(date -Is) no public IPv4 ($IP)"; exit 1; }

# The zone: the longest suffix of NAME that is a zone of this token.
ZONE=""; z="$NAME"
while [[ "$z" == *.* ]]; do
  ZONE=$(curl -fsS -m 20 "${auth[@]}" "$API/zones?name=$z" | json "d['result'][0]['id'] if d['result'] else ''")
  [ -n "$ZONE" ] && break
  z=${z#*.}
done
[ -n "$ZONE" ] || { echo "$(date -Is) no zone for $NAME"; exit 1; }

REC=$(curl -fsS -m 20 "${auth[@]}" "$API/zones/$ZONE/dns_records?type=A&name=$NAME" | json "(d['result'][0]['id'] + ' ' + d['result'][0]['content']) if d['result'] else ''")
if [ -z "$REC" ]; then
  curl -fsS -m 20 "${auth[@]}" -X POST "$API/zones/$ZONE/dns_records" \
    -d "{\"type\":\"A\",\"name\":\"$NAME\",\"content\":\"$IP\",\"proxied\":false,\"ttl\":300}" >/dev/null
  echo "$(date -Is) created $NAME -> $IP"
elif [ "${REC#* }" != "$IP" ]; then
  curl -fsS -m 20 "${auth[@]}" -X PATCH "$API/zones/$ZONE/dns_records/${REC%% *}" -d "{\"content\":\"$IP\"}" >/dev/null
  echo "$(date -Is) updated $NAME ${REC#* } -> $IP"
fi
