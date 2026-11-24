# sm64coin launcher (retired)

This one-command launcher (node + local website + wallet) is **no longer built or shipped** in the
releases. Users open the wallet app, which starts its own node; servers run `scripts/sm64coin-node`
(shipped as `sm64coin-node`, with `packaging/sm64coin-node.service`). The source is kept for reference
(it still holds the BitTorrent Mainline DHT lookup in `src/dht.rs`).
