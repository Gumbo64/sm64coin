//! Tip push over plain HTTP: the shared watcher and the SSE listener in front of a node's RPC.
//!
//!   cargo run --example ssesrv --features tip-server -- http://127.0.0.1:16481 127.0.0.1:16490
use std::sync::Arc;
use std::time::Duration;

use sm64coin_lc::http::HttpUpstream;
use sm64coin_lc::sse::{SseConfig, serve};
use sm64coin_lc::tipfeed::{TipFeed, TipFeedConfig};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut a = std::env::args().skip(1);
    let node = a.next().ok_or("usage: ssesrv <node rpc url> <bind addr>")?;
    let bind = a.next().unwrap_or("127.0.0.1:0".into()).parse()?;
    let feed = TipFeed::new(Arc::new(HttpUpstream::new(&node)?), TipFeedConfig::default());
    tokio::spawn(feed.clone().run(|| false));
    let (addr, task) = serve(feed.clone(), SseConfig { bind, ..Default::default() }).await?;
    println!("http://{addr}/lc/tip");
    loop {
        tokio::time::sleep(Duration::from_secs(60)).await;
        eprintln!("subscribers {}", feed.subscribers());
        if task.is_finished() {
            return Ok(());
        }
    }
}
