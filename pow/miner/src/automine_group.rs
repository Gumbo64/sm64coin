//! Group mining glue for automine: the iroh group (pow/group) as another transport for what the searches of one
//! block share: the template (the leader's), the first castle step (`step_get`/`step_put`) and the runs.

use super::{Args, rh};
use anyhow::{Result, bail};
use sm64coin_group::{Group, Opts, RunMsg};
use sm64coin_miner::rrt;
use sm64pow::proof::Start;
use sm64pow::sm64_binds::SaveState;
use std::sync::{Arc, Mutex};

/// What a received run is replayed against: the current template's seed, parameters and parent end state.
pub struct CtxInner {
    tid: u64,
    parent: Option<SaveState>,
    seed: sm64pow::Seed,
    params: sm64pow::Params,
}
pub type Ctx = Arc<Mutex<Option<CtxInner>>>;

pub fn set_ctx(ctx: &Ctx, template_json: &str, parent: Option<SaveState>, seed: sm64pow::Seed, params: sm64pow::Params) {
    *ctx.lock().unwrap() = Some(CtxInner { tid: sm64coin_group::template_id(template_json), parent, seed, params });
}

fn kind(start: Start) -> &'static str {
    if start == Start::Parent { "parent" } else { "power-on" }
}

fn pads_of(raw: &[u8]) -> Vec<sm64pow::sm64_binds::GamePad> {
    raw.chunks_exact(4).map(sm64pow::proof::pad_from).collect()
}

/// The sm64pow check of a received run: its proof decodes (the same replay a node does: perturbed from the start,
/// reaching the progress point) for this template, with the claimed length and start.
fn verify(c: &CtxInner, r: &RunMsg) -> bool {
    decode(c.parent.as_ref(), &c.seed, &c.params, r).is_some()
}

/// The run's pads, if its proof is valid for the template.
fn decode(parent: Option<&SaveState>, seed: &sm64pow::Seed, params: &sm64pow::Params, r: &RunMsg) -> Option<Vec<sm64pow::sm64_binds::GamePad>> {
    let start = match r.start.as_str() {
        "parent" => Start::Parent,
        "power-on" => Start::PowerOn,
        _ => return None,
    };
    let parent = if start == Start::Parent { Some(parent?) } else { None };
    let pr = sm64pow::decode_proof_any(&r.proof, parent, seed, params).ok()?;
    if pr.frames != r.frames || pr.start != start {
        return None;
    }
    let pads = sm64pow::played_from_proof_at(parent, seed, params, &pr).ok()?;
    (pads.len() as u32 == r.frames).then_some(pads)
}

/// Starts or joins the group the command line asks for.
pub fn setup(a: &Args, ctx: &Ctx) -> Result<Option<Group>> {
    if a.group.is_none() && !a.group_new {
        return Ok(None);
    }
    let name = if a.group_name.is_empty() { std::env::var("HOSTNAME").or_else(|_| std::fs::read_to_string("/etc/hostname").map(|s| s.trim().to_string())).unwrap_or_else(|_| "miner".into()) } else { a.group_name.clone() };
    let o = Opts { name, relay: a.group_relay.clone(), unverified_ok: false, platform: "native".into() };
    let g = match &a.group {
        Some(code) => sm64coin_group::blocking::join(code, o)?,
        None => sm64coin_group::blocking::create(o)?,
    };
    if g.is_leader() {
        println!("group: started as the leader. Team code (a secret: anyone with it joins):\n\n  {}\n\n  other miners: sm64coin-automine --group {} --daemon <node>", g.code(), g.code());
    } else {
        println!("group: joined as {} (leader's templates; reward goes to the group's address)", g.id());
        if a.address.starts_with("SM6") {
            println!("group: --address ignored, the template's address is the leader's");
        }
    }
    let c = ctx.clone();
    g.set_verifier(Arc::new(move |json, r| {
        let guard = c.lock().unwrap();
        match guard.as_ref() {
            Some(cx) if cx.tid == sm64coin_group::template_id(json) => verify(cx, r),
            _ => false,
        }
    }));
    // Log what happens in the group.
    let gl = g.clone();
    std::thread::spawn(move || {
        let mut tgen = 0;
        loop {
            std::thread::sleep(std::time::Duration::from_secs(1));
            let (n, lines) = gl.log_since(tgen);
            tgen = n;
            for l in lines {
                println!("group: {l}");
            }
        }
    });
    if a.group_new && a.group.is_some() {
        bail!("unreachable");
    }
    Ok(Some(g))
}

/// A number that differs between the members (their searches use different seeds).
pub fn member_salt(g: &Group) -> u64 {
    let h = sm64coin_group::template_id(&g.id());
    h % 1_000_000 * 1000
}

/// The channel the searches of a block reach the group by: the first castle step.
pub fn exchange(g: &Group) -> rrt::Exchange {
    let g = g.clone();
    Arc::new(move |kind, payload| match kind {
        "step_get" => g.get_step(payload),
        "step_put" => {
            if let Some(k) = serde_json::from_str::<serde_json::Value>(payload).ok().and_then(|v| v.get("key").and_then(|k| k.as_str().map(str::to_string))) {
                g.put_step(&k, payload);
            }
            None
        }
        _ => None,
    })
}

/// Tells the group about a run we found (with its sealed proof).
pub fn share(g: &Group, start: Start, m: &rh::Mined, proof: &[u8]) {
    g.put_run(kind(start), m.pads.len() as u32, proof, &m.keys, &[]);
}

/// The group's best (verified) run for this start kind, as a run of ours (its pads come from replaying the proof).
pub fn adopt(g: &Group, start: Start, parent: Option<&SaveState>, seed: &sm64pow::Seed, params: &sm64pow::Params) -> Option<rh::Mined> {
    let r = g.best_run(kind(start))?;
    let pads = decode(parent, seed, params, &r)?;
    Some(rh::Mined { pads, sim_frames: 0, seconds: 0.0, keys: r.keys })
}
