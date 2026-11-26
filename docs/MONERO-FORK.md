# What is Monero and what is new

SM64Coin is a fork of **Monero v0.18.5.4**, the official upstream release. The cryptography that protects
your coins, your keys and your privacy is Monero's own code, **unchanged**. What is new is the game: how a
block is won (by playing Super Mario 64) and the tools around it. This page lists exactly which is which,
and how to check it yourself.

## The node and wallet (C++): Monero with small, marked changes

Repository: [Gumbo64/sm64coin-monero](https://github.com/Gumbo64/sm64coin-monero), branch `sm64coin`.

**[See every changed line against Monero v0.18.5.4](https://github.com/Gumbo64/sm64coin-monero/compare/v0.18.5.4...sm64coin)**:
61 files, +5,066 / −247 lines. Every change is marked with a `SM64COIN` comment.

**Not changed at all (0 lines):**

| Monero code | What it does |
|---|---|
| `src/crypto/` | keys, hashing, signatures, key images |
| `src/ringct/` | RingCT: hidden amounts, ring signatures, range proofs |
| `src/device/` | key handling (software and hardware wallets) |
| `src/mnemonics/` | the 25 seed words |
| `src/common/` | shared utilities |

Transaction signing, stealth addresses, ring signatures, hidden amounts and seed words are therefore exactly Monero's.

**What changed, and why:**

| Area | Change |
|---|---|
| `src/sm64coin/` (new, 955 lines) | A thin adapter that calls the proof-of-play rules (Rust, below) through a C ABI |
| `src/cryptonote_core/` | Block validation asks the adapter whether a block's Mario run is valid, instead of checking a RandomX hash; the in-node leaderboard |
| `src/cryptonote_basic/` (66 lines) | The block carries its controller-input proof; difficulty is a frame limit |
| `src/cryptonote_protocol/`, `src/p2p/` | Gossip of proofs, codecs, evidence and leaderboard entries between nodes |
| `src/rpc/` | Extra `sm64_*` fields and calls for miners and the website |
| `src/cryptonote_config.h` | A new network: name, address prefix `SM64`, ports, genesis block, a 6-block coinbase lock |
| `src/wallet/` (42 lines) | File-format names say "SM64Coin" instead of "Monero"; the hard-fork table lookup handles a new chain; new wallets scan from block 0 |

## The proof-of-play rules (Rust): new code

`pow/sm64pow` in this repository decides whether a Mario run seals a block: replaying the inputs in the
game, the controller "pokes", the frame-limit auction and proof formats. It is new, written for this project
(much of it with AI assistance), tested, but **not independently audited**. It can only decide which block
is valid; it never sees or handles keys or coins.

## The browser wallet: Monero's wallet compiled to WebAssembly

The website's wallet is Monero's `wallet2` (the same C++ as the desktop wallet above) compiled to
WebAssembly by the [monero-ts](https://github.com/monero-ecosystem/monero-ts) project, at commit
`919ee6453136ad7a7ceca3d77822436be93758cb`. Our changes to it are build glue only
([`web/wallet-wasm/monero-ts-sm64.patch`](../web/wallet-wasm/monero-ts-sm64.patch), 149 lines: CMake, the
HTTP client, the bridge, webpack) plus a 53-line shim that computes proof ids. Keys are created, stored
(encrypted, in your browser) and used by that Monero code.

What differs from a desktop wallet is *where* things live: the keys sit in browser storage rather than in a
file, and the wallet usually talks to a public node rather than your own. For the most privacy, run your
own node.

## The desktop app (Qt): Monero GUI, rebranded

Repository: [Gumbo64/sm64coin-gui](https://github.com/Gumbo64/sm64coin-gui), branch `sm64coin`, a fork of
monero-gui v0.18.5.4. **[See every change](https://github.com/Gumbo64/sm64coin-gui/compare/v0.18.5.4...sm64coin)**.
Most of it is renaming (translations, images, text); the new parts are the Mining and Leaderboard pages
and the local node / ROM helpers.

## The website

The website (`web/`) is new code. Everything security-sensitive in it runs inside the Monero wallet code
above; the site's own job is the game, the explorer, mining and the leaderboard. Its security setup (strict
content-security policy, no third-party scripts) is described in [WEB-SECURITY.md](WEB-SECURITY.md).

## Check it yourself

```sh
git clone https://github.com/Gumbo64/sm64coin-monero && cd sm64coin-monero
git diff --stat v0.18.5.4 sm64coin                                  # every changed file
git diff v0.18.5.4 sm64coin -- src/crypto src/ringct src/device src/mnemonics   # prints nothing
git grep -n SM64COIN sm64coin                                       # every marked change
```

`v0.18.5.4` here is Monero's own release tag (signed by the Monero maintainers; `git verify-tag v0.18.5.4`
with their key from [monero-project](https://github.com/monero-project/monero/tree/master/utils/gpg_keys)).
