//! The automated miner's route library.
//!
//! Every block on the chain carries a winning run (its proof), and replaying
//! those pads without the perturbation reproduces it exactly, so every block
//! is a free reference route: human runs from the web miner, other bots',
//! ours. The library holds those plus any routes given as files, and picks
//! which to follow per attempt with UCB1 over each route's record (wins and
//! attempts, persisted), so routes that survive the perturbation get used
//! more and new ones still get tried.

use crate::m64::load_m64;
use crate::rpc::Daemon;
use crate::tracker::Reference;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};

#[derive(Default, Clone, Serialize, Deserialize)]
pub struct Record {
    pub tries: u64,
    pub wins: u64,
    /// Sum over failed tries of the fraction of the route reached.
    pub progress: f64,
}

pub struct Route {
    pub name: String,
    pub reference: Reference,
}

pub struct Library {
    pub routes: Vec<Route>,
    pub stats: BTreeMap<String, Record>,
    stats_path: Option<String>,
    seen_blocks: HashSet<String>,
    scanned_to: u64,
    /// Routes longer than this many frames are not worth following.
    pub max_frames: usize,
}

impl Library {
    pub fn new(stats_path: Option<String>) -> Library {
        let stats = stats_path
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        Library { routes: vec![], stats, stats_path, seen_blocks: HashSet::new(), scanned_to: 1, max_frames: 9000 }
    }

    pub fn add_file(&mut self, path: &str) -> Result<()> {
        let r = Reference::record(&load_m64(path)?)?;
        let name = format!("file:{}", std::path::Path::new(path).file_name().unwrap_or_default().to_string_lossy());
        println!("route {name}: {} frames to success", r.inputs.len());
        self.routes.push(Route { name, reference: r });
        Ok(())
    }

    /// Adds the runs of blocks not seen yet (one RPC call per new block).
    pub fn harvest(&mut self, daemon: &Daemon) -> Result<usize> {
        let height = daemon.height()?;
        let mut added = 0;
        for h in self.scanned_to.max(1)..height {
            let (hash, pads) = match daemon.block_run(h) {
                Ok(x) => x,
                Err(e) => {
                    println!("  route harvest: block {h}: {e}");
                    continue;
                }
            };
            if !self.seen_blocks.insert(hash.clone()) || pads.is_empty() || pads.len() > self.max_frames {
                continue;
            }
            if self.routes.iter().any(|r| r.reference.inputs == pads) {
                continue; // same run already loaded (e.g. from a file)
            }
            match Reference::record(&pads) {
                Ok(r) => {
                    let name = format!("block:{h}:{}", &hash[..8]);
                    println!("  route {name}: {} frames (from the chain)", r.inputs.len());
                    self.routes.push(Route { name, reference: r });
                    added += 1;
                }
                Err(e) => println!("  block {h}'s run doesn't replay unperturbed ({e}); skipped"),
            }
        }
        self.scanned_to = height;
        Ok(added)
    }

    /// Route indices in the order to try them: UCB1 on win rate, with a
    /// little credit for how far failed tries got, and shorter routes first
    /// among equals.
    pub fn ranked(&self) -> Vec<usize> {
        let total: u64 = self.routes.iter().map(|r| self.stats.get(&r.name).map_or(0, |s| s.tries)).sum();
        let ln = ((total + 1) as f64).ln();
        let mut scored: Vec<(f64, usize)> = self
            .routes
            .iter()
            .enumerate()
            .map(|(i, r)| {
                let s = self.stats.get(&r.name).cloned().unwrap_or_default();
                let n = s.tries as f64;
                let score = if s.tries == 0 {
                    f64::INFINITY
                } else {
                    let rate = (s.wins as f64 + 0.25 * s.progress / n.max(1.0)) / n;
                    rate + (2.0 * ln / n).sqrt()
                };
                (score - r.reference.inputs.len() as f64 * 1e-7, i)
            })
            .collect();
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        scored.into_iter().map(|(_, i)| i).collect()
    }

    pub fn record(&mut self, i: usize, won: bool, progress: f64) {
        let name = self.routes[i].name.clone();
        let s = self.stats.entry(name).or_default();
        s.tries += 1;
        if won {
            s.wins += 1;
        } else {
            s.progress += progress.clamp(0.0, 1.0);
        }
        if let Some(p) = &self.stats_path {
            if let Ok(j) = serde_json::to_string_pretty(&self.stats) {
                let _ = std::fs::write(p, j);
            }
        }
    }

    pub fn summary(&self) -> String {
        self.ranked()
            .iter()
            .take(6)
            .map(|&i| {
                let r = &self.routes[i];
                let s = self.stats.get(&r.name).cloned().unwrap_or_default();
                format!("{} {}/{}", r.name, s.wins, s.tries)
            })
            .collect::<Vec<_>>()
            .join(", ")
    }
}
