//! Campaign lifecycle: a campaign that ends before the caller subscribes (a setup error right after `start`) still delivers
//! its `Stopped` to a late subscriber, so automine and the Android app never wait forever. No daemon is contacted (the
//! payout key check fails first). Needs the ROM (SM64COIN_ROM or ./baserom.us.z64); skipped without it.
use sm64coin_miner::campaign::{Campaign, CampaignConfig, Event};
use std::time::Duration;

#[test]
fn late_subscriber_gets_stopped() {
    let Ok(rom) = std::fs::read(std::env::var("SM64COIN_ROM").unwrap_or_else(|_| "baserom.us.z64".into())) else {
        eprintln!("no ROM; skipped");
        return;
    };
    let dir = std::env::temp_dir().join(format!("sm64coin-campaign-test-{}", std::process::id()));
    let addr = "SM64RtPqsHM63LZn2u9Myx9HtC4kjhu1h98SRtJz9USB7NZAoCMdZSBD7D1L2DWu9uCYZFUSVKytreuzEMJ6VjBU2tbjKqevFH";
    // A closed loopback port: never reached anyway (the bad view secret ends the campaign first).
    let mut cfg = CampaignConfig::new("http://127.0.0.1:1", addr, rom, dir.clone());
    cfg.payout_view_secret = Some("not hex".into());
    let c = Campaign::start(cfg).expect("start");
    c.join(); // ended before anyone subscribed
    let rx = c.subscribe();
    match rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Event::Stopped(Some(e))) => assert!(!e.contains("not hex"), "the secret must not be echoed: {e}"),
        other => panic!("expected Stopped(Some(_)), got {:?}", other.map(|_| "another event")),
    }
    let _ = std::fs::remove_dir_all(dir);
}
