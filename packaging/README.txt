SM64Coin @VERSION@
==================

SM64Coin is a Monero fork whose blocks are sealed by playing Super Mario 64.

What you need
  * Your own US Super Mario 64 ROM: baserom.us.z64
    (SHA-1 9bef1128717f958171a4afac3ed78ee2bb4e86ce). It is not included.

Run it
  1. Put baserom.us.z64 in this folder (or in ~/.sm64coin/ , or pass its path:
     sm64coin /path/to/rom.z64).
  2. Run  sm64coin   (Windows: double-click sm64coin.exe).
  It starts the node, joins the network, opens the website in your browser and
  starts the wallet app. Press Ctrl-C in its window to stop everything.

Where things are
  ~/.sm64coin/node      the blockchain          ~/.sm64coin/logs   logs
  ~/SM64Coin/wallets    wallets (wallet app)    seeds.txt          known peers (edit it)

Useful options (sm64coin --help)
  --peer HOST:PORT      join through this node (seeds.txt is always read)
  --no-browser          do not open the browser     --no-wallet   no wallet app
  --web-port N          website port (default 5181)
  --leaderboard-port N  also run the leaderboard server (if present in this bundle)
  --mine SM64...        also run the automated miner (if present in this bundle)
  --solo                first node of a brand-new network (do not use otherwise)

Other programs in this folder
  sm64coind             the node itself          sm64coin-wallet-cli / -rpc   command-line wallet
  sm64coin-wallet-gui   the wallet app           sm64coin-leaderboard         optional leaderboard server

Source and issues: https://github.com/Gumbo64/sm64coin
