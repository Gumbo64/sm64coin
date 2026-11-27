//! Building iroh endpoints: relay map (SM64Coin relays first, n0's public relays only as a last
//! resort), address lookup (off for tests, n0 DNS/pkarr, or mainline DHT), announcement signing.
use std::net::SocketAddr;
use std::time::Duration;

use iroh::{Endpoint, EndpointAddr, RelayMode, RelayUrl, SecretKey, TransportAddr, endpoint::presets};

use crate::proto::{ANNOUNCEMENT_TTL_SECS, NodeAnnouncement};

/// How an endpoint is found by others (and finds them).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Discovery {
    /// Nothing is published or resolved: static addressing only (tests, closed networks).
    None,
    /// n0's pkarr/DNS services (works in browsers; a fallback).
    N0,
    /// Mainline DHT (desktop and nodes; never Android).
    #[cfg(feature = "dht")]
    Dht,
}

#[derive(Clone, Debug)]
pub struct EndpointOptions {
    pub secret_key: Option<SecretKey>,
    /// SM64Coin relays (the Pi's, volunteers').
    pub relays: Vec<RelayUrl>,
    /// Add n0's public relays only if none of `relays` has connected after `n0_fallback_after`.
    pub n0_fallback: bool,
    pub n0_fallback_after: Duration,
    pub discovery: Discovery,
    pub bind_addr: Option<SocketAddr>,
    /// If non-empty, relay TLS trusts ONLY these DER certificates (tests with a self-signed relay).
    pub only_trust_certs: Vec<Vec<u8>>,
    pub alpns: Vec<Vec<u8>>,
}

impl Default for EndpointOptions {
    fn default() -> Self {
        Self {
            secret_key: None,
            relays: vec![],
            n0_fallback: true,
            n0_fallback_after: Duration::from_secs(8),
            discovery: Discovery::None,
            bind_addr: None,
            only_trust_certs: vec![],
            alpns: vec![],
        }
    }
}

/// Binds an endpoint. With `relays` empty and `n0_fallback` set, n0's relays are used straight away.
pub async fn build_endpoint(opts: EndpointOptions) -> anyhow_free::Result<Endpoint> {
    let relay_mode = if opts.relays.is_empty() {
        if opts.n0_fallback { RelayMode::Default } else { RelayMode::Disabled }
    } else {
        RelayMode::custom(opts.relays.clone())
    };
    let mut b = Endpoint::builder(presets::Minimal).relay_mode(relay_mode).alpns(opts.alpns.clone());
    if let Some(k) = opts.secret_key.clone() {
        b = b.secret_key(k);
    }
    if !opts.only_trust_certs.is_empty() {
        let roots = opts.only_trust_certs.iter().cloned().map(rustls_pki_types::CertificateDer::from);
        b = b.ca_tls_config(iroh_relay::tls::CaTlsConfig::custom_roots(roots));
    }
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(a) = opts.bind_addr {
        b = b.clear_ip_transports().bind_addr(a).map_err(|e| anyhow_free::err(e))?;
    }
    match opts.discovery {
        Discovery::None => {}
        Discovery::N0 => {
            b = b.address_lookup(iroh::address_lookup::PkarrPublisher::n0_dns()).address_lookup(iroh::address_lookup::PkarrResolver::n0_dns());
            #[cfg(not(target_arch = "wasm32"))]
            {
                b = b.address_lookup(iroh::address_lookup::DnsAddressLookup::n0_dns());
            }
        }
        #[cfg(feature = "dht")]
        Discovery::Dht => {
            b = b.address_lookup(iroh_mainline_address_lookup::DhtAddressLookup::builder());
        }
    }
    let ep = b.bind().await.map_err(|e| anyhow_free::err(e))?;
    if !opts.relays.is_empty() && opts.n0_fallback {
        let (ep2, after) = (ep.clone(), opts.n0_fallback_after);
        n0_future::task::spawn(async move {
            n0_future::time::sleep(after).await;
            let connected = iroh::Watcher::get(&mut ep2.home_relay_status()).iter().any(|r| r.is_connected());
            if !connected && !ep2.is_closed() {
                for cfg in iroh::defaults::prod::default_relay_map().relays::<Vec<_>>() {
                    ep2.insert_relay(cfg.url.clone(), cfg).await;
                }
                tracing::warn!("no SM64Coin relay reachable: n0's public relays added as a last resort");
            }
        });
    }
    Ok(ep)
}

/// Tiny error shim (the crate avoids an anyhow dependency).
pub mod anyhow_free {
    pub type Result<T> = std::result::Result<T, Error>;
    #[derive(Debug, thiserror::Error)]
    #[error("{0}")]
    pub struct Error(pub String);
    pub fn err(e: impl std::fmt::Display) -> Error {
        Error(e.to_string())
    }
}

/// Creates a signed [`NodeAnnouncement`] for `endpoint_addr` (valid for 24 h from `now` seconds).
pub fn sign_announcement(secret: &SecretKey, addr: &EndpointAddr, https_rpc: Option<String>, height: u64, flags: u32, version: u32, now: u64) -> NodeAnnouncement {
    let mut relay_url = None;
    let mut direct_addrs = vec![];
    for t in addr.addrs.iter() {
        match t {
            TransportAddr::Relay(r) => relay_url = Some(r.to_string()),
            TransportAddr::Ip(i) => direct_addrs.push(i.to_string()),
            _ => {}
        }
    }
    let mut a = NodeAnnouncement {
        endpoint_id: *secret.public().as_bytes(),
        relay_url,
        https_rpc,
        direct_addrs,
        height,
        flags,
        version,
        expires: now + ANNOUNCEMENT_TTL_SECS,
        signature: vec![],
    };
    a.signature = secret.sign(&a.signing_bytes()).to_bytes().to_vec();
    a
}
