//! The tip, watched ONCE per server and fanned out to every subscriber (Server-Sent Events over plain HTTP: sse.rs).
//!
//! One poller asks the local node (`get_info`, plus the tip block's header when the tip changed) every `poll` while anybody is
//! subscribed and every `idle_poll` otherwise, and publishes through a `watch` channel. Subscriptions are counted per key (an
//! IP address) and in total; [`TipFeed::acquire`] refuses beyond the caps.
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::exec::{self, Upstream};
use crate::proto::*;

#[derive(Clone, Debug)]
pub struct TipFeedConfig {
    /// Poll period of the local node while somebody is subscribed (it is local: 1 s is cheap).
    pub poll: Duration,
    /// ... and while nobody is.
    pub idle_poll: Duration,
    /// Silence on a subscription after which a keepalive is sent.
    pub keepalive: Duration,
    pub max_total: usize,
}

impl Default for TipFeedConfig {
    fn default() -> Self {
        Self { poll: Duration::from_secs(1), idle_poll: Duration::from_secs(5), keepalive: Duration::from_secs(KEEPALIVE_SECS), max_total: MAX_SUBS_TOTAL }
    }
}

pub struct TipFeed {
    pub cfg: TipFeedConfig,
    upstream: Arc<dyn Upstream>,
    tx: tokio::sync::watch::Sender<Option<TipInfo>>,
    total: AtomicUsize,
    per_key: Mutex<HashMap<String, usize>>,
}

/// A held subscription slot; dropping it frees the slot.
pub struct SubGuard {
    feed: Arc<TipFeed>,
    key: String,
}

impl Drop for SubGuard {
    fn drop(&mut self) {
        self.feed.total.fetch_sub(1, Ordering::AcqRel);
        let mut m = self.feed.per_key.lock().unwrap();
        if let Some(n) = m.get_mut(&self.key) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                m.remove(&self.key);
            }
        }
    }
}

pub fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

impl TipFeed {
    pub fn new(upstream: Arc<dyn Upstream>, cfg: TipFeedConfig) -> Arc<Self> {
        Arc::new(Self { cfg, upstream, tx: tokio::sync::watch::channel(None).0, total: AtomicUsize::new(0), per_key: Default::default() })
    }
    pub fn current(&self) -> Option<TipInfo> {
        self.tx.borrow().clone()
    }
    pub fn subscribe(&self) -> tokio::sync::watch::Receiver<Option<TipInfo>> {
        self.tx.subscribe()
    }
    pub fn subscribers(&self) -> usize {
        self.total.load(Ordering::Relaxed)
    }
    /// Takes a subscription slot for `key` (at most `max_per_key` at once for it, `cfg.max_total` in all); None: over a cap.
    pub fn acquire(self: &Arc<Self>, key: &str, max_per_key: usize) -> Option<SubGuard> {
        let mut m = self.per_key.lock().unwrap();
        let mine = m.get(key).copied().unwrap_or(0);
        if mine >= max_per_key || self.total.load(Ordering::Acquire) >= self.cfg.max_total {
            return None;
        }
        m.insert(key.to_string(), mine + 1);
        self.total.fetch_add(1, Ordering::AcqRel);
        Some(SubGuard { feed: self.clone(), key: key.to_string() })
    }
    /// One look at the node; publishes the tip when it is a new block (or difficulty). Returns the tip read, if any.
    pub async fn refresh(&self) -> Option<TipInfo> {
        let known = self.current();
        let t = exec::tip_info(&*self.upstream, known.as_ref(), now_secs(), Duration::from_secs(5)).await.ok()?;
        self.tx.send_if_modified(|cur| {
            if cur.as_ref().is_some_and(|c| c.hash == t.hash && c.difficulty == t.difficulty) {
                return false;
            }
            *cur = Some(t.clone());
            true
        });
        Some(t)
    }
    /// Runs the poller until `done()` says so.
    pub async fn run(self: Arc<Self>, done: impl Fn() -> bool + Send) {
        while !done() {
            self.refresh().await;
            tokio::time::sleep(if self.subscribers() > 0 { self.cfg.poll } else { self.cfg.idle_poll }).await;
        }
    }
}
