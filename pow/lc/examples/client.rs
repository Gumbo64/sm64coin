//! Example light client: asks one or more servers for the chain tip and block headers, with failover.
//!
//!   cargo run --example client --features relay -- --network <genesis hash hex> \
//!       iroh:<endpoint id hex>@<relay url> https://some-node.example
//!
//! Servers are `iroh:<id>[@<relay>]` (QUIC, end-to-end authenticated) or `http(s)://` JSON-RPC nodes:
//! the same `Client` and the same `Request`s work over both transports.
use std::sync::Arc;

use sm64coin_lc::client::{Client, IrohConnector, READ_TIMEOUT, SystemEnv};
use sm64coin_lc::endpoint::{EndpointOptions, build_endpoint};
use sm64coin_lc::nodes::{NodeList, Source, parse_node_spec};
use sm64coin_lc::proto::{Request, Response};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let mut network = None;
    let mut list = NodeList::new();
    while let Some(a) = args.next() {
        if a == "--network" {
            network = args.next();
        } else if let Some(n) = parse_node_spec(&a) {
            list.add(n, Source::Manual, 0);
        } else {
            eprintln!("ignoring {a}");
        }
    }
    let network: [u8; 32] = hex::decode(network.ok_or("--network <genesis hash hex> is required")?)?.try_into().map_err(|_| "network must be 32 bytes")?;
    let ep = build_endpoint(EndpointOptions::default()).await?;
    let client = Client::new(list, Arc::new(IrohConnector::new(ep, network)), Arc::new(SystemEnv));
    client.refresh_tips(4, READ_TIMEOUT).await;
    let (node, tip) = client.read(&Request::GetTip, READ_TIMEOUT).await?;
    println!("{node}: {tip:?}");
    if let Response::Tip(t) = tip {
        let from = t.height.saturating_sub(4);
        let (_, h) = client.read(&Request::GetHeaders { from, count: 5 }, READ_TIMEOUT).await?;
        println!("headers {from}..: {h:?}");
    }
    println!("node list: {}", client.list_json());
    Ok(())
}
