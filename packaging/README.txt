SM64Coin @VERSION@
==================

SM64Coin is a cryptocurrency whose blocks are sealed by playing Super Mario 64:
a block is valid when a recorded run of the game proves the puzzle of that block.
This folder is the whole program: a wallet app that runs its own node, and the website
that lets you play and mine in your browser.

What you need
  * Your own US Super Mario 64 ROM, named baserom.us.z64
    (SHA-1 9bef1128717f958171a4afac3ed78ee2bb4e86ce). It is NOT included and we cannot
    give it to you. Any file name works if the content is the US ROM in .z64 format
    (byte order 80 37 12 40); SM64Coin checks the content, not the name.
  * Windows 10/11 (64 bit), macOS 15 or newer on Apple silicon (M1 and later),
    or Linux (64 bit Intel/AMD, or ARM such as a Raspberry Pi 4/5 with a 64 bit OS).
  * An internet connection and about 2 GB of free disk space.
  Nothing else has to be installed.

Start it
  1. Unzip / unpack this download to a normal folder (Windows: right-click the .zip,
     "Extract All"; do not run it from inside the zip).
  2. Copy baserom.us.z64 into that folder (next to this README), or into the .sm64coin folder
     of your user folder (see below).
  3. Open the wallet:
       Windows:  double-click  sm64coin-wallet-gui.exe
       macOS:    open  SM64Coin-Wallet.app
       Linux:    ./sm64coin-wallet-gui   (in a terminal, from this folder)

  First time on Windows: SmartScreen may say "Windows protected your PC" because the
  program is not signed. Click "More info", then "Run anyway".
  First time on macOS: the app is not signed by Apple, so Gatekeeper blocks it. Either
  right-click SM64Coin-Wallet.app in Finder, choose Open, then Open again; or run this once
  in the Terminal in this folder:
       xattr -dr com.apple.quarantine .
  Windows firewall: allow access for sm64coind.exe when asked (it needs to talk to other nodes).

What you will see
  * On its first start the wallet asks you to create or restore a wallet. Pick "Create a new
    wallet" and write the 25 words down on paper (they are the only backup!).
  * The wallet starts its own node (sm64coind) by itself. The node checks your ROM, joins the
    network through the seed nodes (seeds.txt) and syncs the blockchain; the wallet shows the
    progress. The first sync can take a while: leave it running.
  * Mining page of the wallet: "Start" runs the automated miner (if sm64coin-automine is in this
    folder); "Play to mine" opens the website where you play Super Mario 64 yourself to mine.
  Closing the wallet stops its node cleanly.

Where things are (your user folder = C:\Users\<you> on Windows, /Users/<you> on macOS)
  <user folder>/.sm64coin/          the node's data and logs; look at the logs first when something
                                   does not work. Delete the blockchain part to resync.
  <user folder>/SM64Coin/wallets   your wallets. KEEP A BACKUP of your 25 words.
  seeds.txt (this folder)          nodes to join through (the network's seed nodes); one host:port
                                   per line, edit freely
  Nothing is written inside this folder except by you.

Running a node on a server (no wallet app)
  A server (e.g. a Raspberry Pi acting as a gateway) runs sm64coind directly through the script
  sm64coin-node (Linux, macOS) or sm64coin-node.bat (Windows, from this folder).
    ./sm64coin-node --help          all options
    ./sm64coin-node                 foreground; ROM next to the script or in ~/.sm64coin;
                                    data in ~/.sm64coin/node; Ctrl-C stops it
    ./sm64coin-node --public-rpc    also serve the restricted (read-only) RPC on 0.0.0.0:16489
    ./sm64coin-node --solo          ONLY for the first node of a brand-new network
    ./sm64coin-node -- --some-sm64coind-flag
  It reads seeds.txt, resolves the host names to IPv4 and passes them to sm64coind.
  Ports: TCP 16480 (p2p) must be reachable from the internet for others to connect to you; RPC
  (16481) and ZMQ (16482) stay on 127.0.0.1.
  Linux with systemd: sm64coin-node.service is a unit for it (instructions in its first lines):
    sudo useradd --system --create-home --home-dir /var/lib/sm64coin --shell /usr/sbin/nologin sm64coin
    sudo mkdir -p /opt/sm64coin && sudo cp -r . /opt/sm64coin/ && sudo cp baserom.us.z64 /opt/sm64coin/
    sudo cp sm64coin-node.service /etc/systemd/system/
    sudo systemctl daemon-reload && sudo systemctl enable --now sm64coin-node
    journalctl -u sm64coin-node -f
  Windows: sm64coin-node.bat does the same in a simpler way (host names are resolved with
  PowerShell; --public-rpc, --solo, --data-dir, --rom, and the variables DATA_DIR ROM P2P_PORT
  RPC_PORT ZMQ_PORT are supported, nothing else).

Troubleshooting
  "Could not find your Super Mario 64 ROM"  -> step 2 above (the node script lists what it found).
  "port ... is in use"  -> another copy is running; close it.
  The node says "Syncing" for a long time at first: that is normal, leave it running.
  No peers after a minute: the node can't reach the seed nodes. Check the internet connection and
  that a firewall lets sm64coind connect out (TCP port 16480).
  Send the log files in the .sm64coin folder when you ask for help.

Other programs in this folder
  sm64coin-wallet-gui   the wallet app (macOS: SM64Coin-Wallet.app); starts sm64coind for you
  sm64coind             the node itself            sm64coin-wallet-cli / sm64coin-wallet-rpc   command-line wallet
  sm64coin-node         runs the node on a server (see above)
  sm64coin-leaderboard  leaderboard server (shares the top 50 with other servers; joining shares your address + view key with that network, use a dedicated mining address)
  sm64coin-lc           light-client service: run it beside a node (public address, UDP + TCP reachable) so phones, the apps
                        and browsers can read the chain through it instead of one public node. It answers over iroh (QUIC),
                        and turns itself into a relay for others when the machine is reachable from outside and has a
                        Let's Encrypt domain (--tls acme --acme-domain your.domain --acme-production). It reads the node's
                        RESTRICTED RPC only: start the node with RESTRICTED_PORT=16489 (sm64coin-node --restricted-port 16489).
                        Start:  ./sm64coin-lc serve --node-rpc http://127.0.0.1:16489 --data-dir ~/.sm64coin/lc
                        Linux:  sm64coin-lc.service (systemd unit, see its header). `sm64coin --lc` runs the same service
                        inside the launcher. Details: pow/lc/README.md and docs/LIGHT-CLIENTS.md in the source.
  sm64coin-automine     the automated miner (when present)

Verify your download against SHA256SUMS on the release page.
Source and issues: https://github.com/Gumbo64/sm64coin
