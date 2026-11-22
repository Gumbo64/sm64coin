//! The engine switches the seg120 campaign sets (campaign.py `ENV`), applied
//! to a `Config` the way `obrrt2`'s main does (run/src/main.rs): only those
//! the campaign uses, copied from there.
use anyhow::Result;
use obrrt_search::{Config, dense, engine, tree};
use std::sync::atomic::Ordering::Relaxed;

pub fn apply(cfg: &mut Config, mem_mb: usize) -> Result<()> {
    cfg.soft_dedup = true; // OBRRT_SOFT_DEDUP=1
    cfg.mem_budget = mem_mb * 1_000_000; // OBRRT_MEM_MB
    cfg.round_plans = 64; // OBRRT_ROUND=64
    cfg.pick_by_b = false; // OBRRT_PICK_BY_B=0
    cfg.chain_route = true; // OBRRT_CHAIN=route
    cfg.sampler_rate = true; // OBRRT_SAMPLER=rate
    cfg.kinship_mult = 10.0; // OBRRT_KINSHIP_MULT=10
    engine::STARSET.store(true, Relaxed); // OBRRT_STARSET_KEY=1
    tree::KEEP_WIP.store(true, Relaxed); // OBRRT_KEEP_WIP=1
    // OBRRT_ADD=last_gen: dense adds with sparse saves (OBRRT_REPLAY_MAX defaults to 150).
    let modes = dense::Modes::parse("last_gen").map_err(anyhow::Error::msg)?;
    tree::REPLAY_MAX.store(150, Relaxed);
    tree::DENSE_GRID.store(64, Relaxed);
    dense::configure(Some(dense::Settings { modes, grid: 64.0, cap: if modes.all { 256 } else { 0 }, ne: true, mem_objects: true, every: 1, per_obj: false, air: true }));
    Ok(())
}
