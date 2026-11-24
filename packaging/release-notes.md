## SM64Coin @TAG@

**Download, put your ROM next to it, run `sm64coin`.**

1. Download the bundle for your computer:
   - `sm64coin-@VERSION@-linux-x86_64.tar.gz`: Linux on a PC (Intel/AMD, 64-bit)
   - `sm64coin-@VERSION@-linux-aarch64.tar.gz`: Linux on ARM, e.g. Raspberry Pi 4/5 with a 64-bit OS
2. Unpack it (`tar xzf sm64coin-*.tar.gz`) and open the folder.
3. Copy your own US Super Mario 64 ROM into it, named `baserom.us.z64`
   (SHA-1 `9bef1128717f958171a4afac3ed78ee2bb4e86ce`; it is not included).
4. Run `./sm64coin`. It joins the network, opens the website in your browser and starts the wallet app.
   Press Ctrl-C to stop.

Nothing else needs to be installed. `README.txt` in the bundle lists the options (`sm64coin --help`).
Verify a download with `sha256sum -c SHA256SUMS --ignore-missing`.

Not in this release: Windows and macOS builds, and the automated miner (`sm64coin-automine`).

### Changes

(edit before publishing)

<!--
  Update notices: the wallet app, the website, sm64coin-node and sm64coin-automine look at the latest
  release (GitHub API, cached 6 h) and tell users when theirs is older. To make the notice a stronger
  warning ("update required"), add a line consisting of only the words  Update: required  (case does
  not matter) to the release body when you publish. Only a whole line counts, so this comment, which
  is published with the notes, does not trigger it.
  Opt out: SM64COIN_NO_UPDATE_CHECK=1 (programs), a setting in the wallet app.
-->
