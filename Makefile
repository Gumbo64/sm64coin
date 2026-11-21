.PHONY: build build-monero build-pow build-web node wallet mine web stop test clean-data

build: build-monero build-pow build-web links

build-monero:
	mkdir -p monero/build/release
	cd monero/build/release && cmake -D CMAKE_BUILD_TYPE=Release -D BUILD_TESTS=OFF -D USE_DEVICE_TREZOR=OFF -D MANUAL_SUBMODULES=1 ../.. && $(MAKE) -j daemon simplewallet wallet_rpc_server

build-pow:
	cd pow && cargo build --release -p sm64coin-miner --bin automine

build-web:
	cd web && npm install && npm run build

node:
	scripts/start-node.sh

wallet:
	scripts/start-wallet-rpc.sh

mine:
	scripts/automine.sh $(ADDR)

web:
	scripts/web.sh

stop:
	scripts/stop.sh

test:
	cd pow && cargo test -p sm64pow

clean-data:
	scripts/stop.sh
	rm -rf data
	rm -f run/node.pid run/node.log run/wallet-rpc.pid run/wallet-rpc.log run/node-daemon.log run/wallet-rpc-daemon.log

# sm64coin-named commands in bin/ (symlinks to the upstream-named binaries, so
# the fork doesn't rename build targets).
links:
	mkdir -p bin
	ln -sfn ../monero/build/release/bin/monerod bin/sm64coind
	ln -sfn ../monero/build/release/bin/monero-wallet-cli bin/sm64coin-wallet-cli
	ln -sfn ../monero/build/release/bin/monero-wallet-rpc bin/sm64coin-wallet-rpc
	ln -sfn ../gui/build/bin/monero-wallet-gui bin/sm64coin-wallet-gui
	ln -sfn ../pow/target/release/automine bin/sm64coin-automine
