SM64Coin @VERSION@
==================

SM64Coin is a cryptocurrency whose blocks are sealed by playing Super Mario 64:
a block is valid when a recorded run of the game proves the puzzle of that block.
This folder is the whole program: a node, a wallet, and the website that lets you
play and mine in your browser.

What you need
  * Your own US Super Mario 64 ROM, named baserom.us.z64
    (SHA-1 9bef1128717f958171a4afac3ed78ee2bb4e86ce). It is NOT included and we cannot
    give it to you. Any file name works if the content is the US ROM in .z64 format
    (byte order 80 37 12 40); SM64Coin checks the content, not the name.
  * Windows 10/11 (64 bit), macOS 15 or newer on Apple silicon (M1 and later),
    or Linux (64 bit Intel/AMD, or ARM such as a Raspberry Pi 4/5 with a 64 bit OS).
  * An internet connection and about 2 GB of free disk space.
  Nothing else has to be installed.

Start it (3 steps)
  1. Unzip / unpack this download to a normal folder (Windows: right-click the .zip,
     "Extract All"; do not run it from inside the zip).
  2. Copy baserom.us.z64 into the same folder as sm64coin (next to this README).
     (You can instead put it in the .sm64coin folder of your user folder, see below.)
  3. Start the program:
       Windows:  double-click  sm64coin.exe
       macOS:    open the Terminal, drag this folder in:  cd <the folder>  then  ./sm64coin
                 (or double-click sm64coin after the one-time step below)
       Linux:    ./sm64coin   (in a terminal, from this folder)

  First time on Windows: SmartScreen may say "Windows protected your PC" because the
  program is not signed. Click "More info", then "Run anyway".
  First time on macOS: the program is not signed by Apple, so Gatekeeper blocks it.
  Either right-click sm64coin (and SM64Coin-Wallet.app) in Finder, choose Open, then
  Open again; or run this once in the Terminal in this folder:
       xattr -dr com.apple.quarantine .
  Windows firewall: allow access for sm64coind.exe when asked (it needs to talk to other nodes).

What you will see
  * A window (terminal) with log lines: the ROM is checked, then "Starting the node",
    "Node is up", "Connected to N peers: ..." (the nodes it talks to), and "Syncing: block N of M"
    until the node has caught up with the network.
  * Your web browser opens the SM64Coin website at http://localhost:5181/ (blocks,
    leaderboard, and the page where you play to mine). It talks to your own node.
  * The wallet app (SM64Coin Wallet) opens. On its first start it asks you to create
    or restore a wallet: pick "Create a new wallet", write the 25 words down on paper
    (they are the only backup!), and when it asks for the node choose
    "Use a remote node / connect to a node" with address 127.0.0.1 and port 16481.
    That is the node this program runs for you.
  If the browser or the wallet does not open by itself, open http://localhost:5181/ yourself.

How to stop
  Press Ctrl-C in the terminal window (or close that window). It stops the node and the
  wallet app cleanly; give it up to a minute. Start it again later with the same steps:
  it continues where it stopped.

Where things are (your user folder = C:\Users\<you> on Windows, /Users/<you> on macOS)
  <user folder>/.sm64coin/node     the blockchain (the node's data; you can delete it to resync)
  <user folder>/.sm64coin/logs     log files (node.log, node-daemon.log, wallet-gui.log); look here
                                   first when something does not work
  <user folder>/SM64Coin/wallets   your wallets. KEEP A BACKUP of your 25 words.
  seeds.txt (this folder)          nodes to join through (the network's seed nodes); one host:port
                                   per line, edit freely
  Nothing is written inside this folder except by you.

Troubleshooting
  "Could not find your Super Mario 64 ROM"  -> step 2 above; the message lists what was found.
  "port ... is in use"  -> another copy is running (close it) or use --p2p-port / --rpc-port / --web-port.
  The node says "Syncing" for a long time at first: that is normal, leave it running.
  "Still no peers after a minute": the node can't reach the seed nodes. Check the internet
  connection and that a firewall lets sm64coind connect out (TCP port 16480), or ask someone
  for a node's address and start with  --peer HOST:16480
  Send the files in .sm64coin/logs when you ask for help.

Options (sm64coin --help)
  --peer HOST:PORT      join through this node (seeds.txt is always read)
  --no-browser          do not open the browser     --no-wallet   do not start the wallet app
  --web-port N          website port (default 5181)
  --data-dir DIR        keep the blockchain and logs somewhere else
  --leaderboard-port N  port of the leaderboard server run by default (16490); --no-leaderboard turns it off
  --peer-leaderboard URL  share the leaderboard with this server too (also: leaderboard-peers.txt)
  --public              website and leaderboard reachable by other computers
  --mine SM64...        also run the automated miner (if present in this folder)
  --solo                first node of a brand-new network (do not use otherwise)
  A path to the ROM can be given too:  sm64coin C:\games\sm64.z64

Other programs in this folder
  sm64coind             the node itself            sm64coin-wallet-cli / sm64coin-wallet-rpc   command-line wallet
  sm64coin-wallet-gui   the wallet app (macOS: SM64Coin-Wallet.app)
  sm64coin-leaderboard  leaderboard server (run by sm64coin; shares the top 50 with other servers; joining shares your address + view key with that network, use a dedicated mining address)

Verify your download against SHA256SUMS on the release page.
Source and issues: https://github.com/Gumbo64/sm64coin
