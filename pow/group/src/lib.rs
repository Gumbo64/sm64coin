//! Group mining for SM64Coin: trusted members mine ONE block template together.
//!
//! A group is a set of members who all mine the same template (same coinbase address and extra nonce, so the
//! perturbation seed, which is hashed from the blob, is the same game for everyone). One member is the leader:
//! it fetches the template from its node and announces it (signed with its iroh key, members ignore templates
//! from anyone else); when the chain moves on it announces the new one. Members share exactly what the
//! searches of one block already share locally (measured: independent searches win, periodic tree-node sync gave
//! no gain): the first castle step's outcome (`step_get`/`step_put`, [`Msg::Step`]) and finished or shorter runs
//! ([`Msg::Run`], the same fields the web miner's workers exchange: pads, objective keys, step starts). Whoever
//! finds the winning run submits it; the reward goes to the template's address (the group's).
//!
//! The transport is iroh-gossip over an iroh endpoint (relay + direct, hole-punched). The team code is a
//! ticket: the leader's endpoint address plus a random 32-byte gossip topic. It is a secret: anyone holding it can
//! join. This crate is transport and bookkeeping only; the host verifies runs ([`Group::set_verifier`]) before
//! they are held or submitted.
//!
//! Message framing: [`Envelope`] = {from, body, signature} (postcard); body = {nonce, [`Msg`]}. Every message is
//! signed by its sender and carries a random nonce (gossip delivers a given content once). Messages above
//! [`MAX_MSG`] bytes are dropped on both ends.

use anyhow::{Context, Result, bail};
use bytes::Bytes;
use iroh::{Endpoint, EndpointAddr, EndpointId, PublicKey, RelayMode, RelayUrl, SecretKey, endpoint::presets};
use iroh_gossip::{ALPN, Event, Gossip, TopicId};
use n0_future::StreamExt;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::{Arc, Condvar, Mutex};

/// Wire version: bump when [`Msg`] changes incompatibly.
pub const PROTO: u8 = 1;
/// Largest message accepted or sent (bytes, signed and encoded).
pub const MAX_MSG: usize = 256 * 1024;
/// Largest inflated payload (a step's JSON, a run's pads) accepted.
pub const MAX_INFLATED: usize = 8 * 1024 * 1024;
/// Most steps / templates kept.
const MAX_STEPS: usize = 16;
/// Prefix of the team code.
pub const CODE_PREFIX: &str = "sm64team1-";
/// A member not heard of for this long is dropped from the list (seconds).
const MEMBER_TTL: u64 = 75;
/// Presence period (seconds); the leader re-announces its template at the same rate.
const BEAT: u64 = 20;

/// What is exchanged. `tid` is the template id (see [`template_id`]); messages for another template are dropped.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum Msg {
    /// Presence: display name, search speed (frames/s) and whether the sender wants the current state (a late joiner).
    Hello { proto: u8, name: String, fps: f32, want_state: bool },
    /// The leader's template (the node's `get_block_template` JSON, deflated). Only valid from the leader.
    Template { json: Vec<u8> },
    /// The outcome of a campaign step another search made (rrt `SharedStep` JSON, deflated).
    /// `platform` ("native" / "web"): a step's key is only the same job on the same kind of build (the browser runs the
    /// search with other settings), so steps are only taken from members of the same platform.
    Step { tid: u64, platform: String, key: String, json: Vec<u8> },
    /// A run: the sealed proof (what the block stores: the run's inputs in the chain's codec; the receiver replays it
    /// and gets the pads), the objective keys of its path and the played length at the start of each step (the fields
    /// the web miner's workers share, for biasing searches), the start kind ("parent" / "power-on").
    Run { tid: u64, start: String, frames: u32, proof: Vec<u8>, keys: Vec<String>, starts: Vec<u32> },
    /// Leaving.
    Bye,
}

#[derive(Serialize, Deserialize)]
struct Body {
    nonce: u64,
    msg: Msg,
}

#[derive(Serialize, Deserialize)]
struct Envelope {
    from: PublicKey,
    body: Vec<u8>,
    sig: Vec<u8>,
}

fn seal(key: &SecretKey, msg: Msg) -> Result<Bytes> {
    let body = postcard::to_stdvec(&Body { nonce: rand::random(), msg })?;
    let sig = key.sign(&body).to_bytes().to_vec();
    let out = postcard::to_stdvec(&Envelope { from: key.public(), body, sig })?;
    if out.len() > MAX_MSG {
        bail!("message too large ({} bytes, limit {MAX_MSG})", out.len());
    }
    Ok(out.into())
}

fn open(bytes: &[u8]) -> Result<(PublicKey, Msg)> {
    if bytes.len() > MAX_MSG {
        bail!("message too large");
    }
    let e: Envelope = postcard::from_bytes(bytes).map_err(|e| anyhow::anyhow!("envelope ({} bytes): {e}", bytes.len()))?;
    let sig: [u8; 64] = e.sig.as_slice().try_into().map_err(|_| anyhow::anyhow!("bad signature length"))?;
    e.from.verify(&e.body, &iroh::Signature::from_bytes(&sig)).map_err(|e| anyhow::anyhow!("bad signature: {e}"))?;
    let b: Body = postcard::from_bytes(&e.body).map_err(|er| anyhow::anyhow!("body ({} bytes, from a build of another protocol?): {er}; head {}", e.body.len(), data_encoding::HEXLOWER.encode(&e.body[..e.body.len().min(48)])))?;
    Ok((e.from, b.msg))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_message_round_trips() {
        let key = SecretKey::generate();
        let msgs = vec![
            Msg::Hello { proto: PROTO, name: "n\u{e9}".into(), fps: 12.5, want_state: true },
            Msg::Template { json: deflate(b"{\"a\":1}") },
            Msg::Step { tid: u64::MAX, platform: "native".into(), key: "k".into(), json: deflate(b"{}") },
            Msg::Run { tid: 0xdead_beef_0123_4567, start: "parent".into(), frames: 99, proof: vec![1, 2, 3], keys: vec!["x".into()], starts: vec![0, 5] },
            Msg::Bye,
        ];
        for m in msgs {
            let b = seal(&key, m.clone()).unwrap();
            let (from, back) = open(&b).unwrap();
            assert_eq!(from, key.public());
            assert_eq!(format!("{m:?}"), format!("{back:?}"));
        }
    }

    #[test]
    fn oversize_and_forged_messages_are_refused() {
        let key = SecretKey::generate();
        let big = Msg::Template { json: vec![7u8; MAX_MSG + 1] };
        assert!(seal(&key, big).is_err());
        let mut b = seal(&key, Msg::Bye).unwrap().to_vec();
        let n = b.len();
        b[n - 1] ^= 1; // damage the signature
        assert!(open(&b).is_err());
        assert!(open(&vec![0u8; MAX_MSG + 1]).is_err());
    }
}

pub fn deflate(data: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut z = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::new(6));
    let _ = z.write_all(data);
    z.finish().unwrap_or_default()
}

pub fn inflate(data: &[u8]) -> Result<Vec<u8>> {
    use std::io::Read;
    let mut out = Vec::new();
    flate2::read::DeflateDecoder::new(data).take(MAX_INFLATED as u64 + 1).read_to_end(&mut out)?;
    if out.len() > MAX_INFLATED {
        bail!("inflated payload too large");
    }
    Ok(out)
}

/// The template's id: the first 8 bytes of blake3 over the blob field when the JSON has one (the template as the
/// block is made from: re-fetches of the same blob with a new timestamp stay the same template), else over the JSON.
pub fn template_id(json: &str) -> u64 {
    let blob = serde_json::from_str::<serde_json::Value>(json).ok().and_then(|v| v.get("blocktemplate_blob").and_then(|b| b.as_str().map(str::to_string)));
    let h = blake3::hash(blob.as_deref().unwrap_or(json).as_bytes());
    u64::from_le_bytes(h.as_bytes()[..8].try_into().unwrap())
}

/// Team code contents.
#[derive(Serialize, Deserialize, Clone, Debug)]
struct Ticket {
    proto: u8,
    topic: [u8; 32],
    leader: EndpointAddr,
}

impl Ticket {
    fn encode(&self) -> String {
        let bytes = postcard::to_stdvec(self).expect("ticket encodes");
        format!("{CODE_PREFIX}{}", data_encoding::BASE32_NOPAD.encode(&bytes).to_ascii_lowercase())
    }
    fn decode(code: &str) -> Result<Ticket> {
        let c = code.trim();
        let c = c.strip_prefix(CODE_PREFIX).context("not a team code (expected the sm64team1- prefix)")?;
        let bytes = data_encoding::BASE32_NOPAD.decode(c.to_ascii_uppercase().as_bytes()).context("team code: bad characters")?;
        let t: Ticket = postcard::from_bytes(&bytes).context("team code: damaged")?;
        if t.proto != PROTO {
            bail!("team code is for protocol {} (this build speaks {PROTO})", t.proto);
        }
        Ok(t)
    }
}

/// A run another member (or we) found.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RunMsg {
    pub tid: u64,
    pub start: String,
    pub frames: u32,
    /// The sealed proof (see [`Msg::Run`]).
    pub proof: Vec<u8>,
    pub keys: Vec<String>,
    pub starts: Vec<u32>,
    /// Who found it (endpoint id, short hex).
    pub from: String,
}

impl RunMsg {
    /// Hex of the proof (what the browser's submit takes).
    pub fn proof_hex(&self) -> String {
        data_encoding::HEXLOWER.encode(&self.proof)
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Member {
    pub id: String,
    pub name: String,
    pub fps: f32,
    pub leader: bool,
    pub me: bool,
    /// Seconds since last heard (0 for us).
    pub age: u64,
}

/// Host-side check of a received run before it is held (it replays the pads; see sm64pow::seal_proof_at). Gets the
/// current template JSON.
pub type Verifier = Arc<dyn Fn(&str, &RunMsg) -> bool + Send + Sync>;

#[derive(Default)]
struct State {
    template: Option<(u64, String)>,
    /// Bumped on every new template (waiters).
    template_gen: u64,
    steps: HashMap<(u64, String), String>,
    step_order: Vec<(u64, String)>,
    /// Steps of our platform for the current template, in arrival order (generation, key, json): the browser takes them from here.
    step_log: Vec<(u64, String, String)>,
    step_gen: u64,
    best: HashMap<String, RunMsg>,
    run_gen: u64,
    members: HashMap<EndpointId, (String, f32, n0_future::time::Instant)>,
    log: std::collections::VecDeque<String>,
    log_gen: u64,
    neighbors: usize,
    verifier: Option<Verifier>,
    unverified_ok: bool,
    closed: bool,
}

struct Inner {
    me: EndpointId,
    key: SecretKey,
    leader: EndpointId,
    name: Mutex<String>,
    fps: Mutex<f32>,
    code: String,
    platform: String,
    state: Mutex<State>,
    cv: Condvar,
    out: tokio::sync::mpsc::UnboundedSender<Bytes>,
}

/// A group handle (cheap to clone). All methods are non-blocking except [`Group::wait_template`].
#[derive(Clone)]
pub struct Group {
    inner: Arc<Inner>,
}

/// Options for [`Group::create`] / [`Group::join`].
#[derive(Clone, Debug, Default)]
pub struct Opts {
    pub name: String,
    /// A relay server URL; None: n0's public relays; Some("none"): no relay (direct only: LAN / port-forwarded).
    pub relay: Option<String>,
    /// Hold received runs without a [`Group::set_verifier`] check (the browser: its node verifies at submit).
    pub unverified_ok: bool,
    /// "native" (default) or "web": which steps are shared with this member (see [`Msg::Step`]).
    pub platform: String,
}

fn relay_mode(o: &Opts) -> Result<RelayMode> {
    Ok(match o.relay.as_deref() {
        None | Some("") | Some("default") => RelayMode::Default,
        Some("none") | Some("off") => RelayMode::Disabled,
        Some(u) => RelayMode::custom([RelayUrl::from_str(u).map_err(|e| anyhow::anyhow!("relay url: {e}"))?]),
    })
}

impl Group {
    /// Starts a new group as its leader. Returns when the endpoint is up (the team code is [`Group::code`]).
    pub async fn create(o: Opts) -> Result<Group> {
        let topic: [u8; 32] = rand::random();
        start(o, topic, None).await
    }

    /// Joins the group a team code names.
    pub async fn join(code: &str, o: Opts) -> Result<Group> {
        let t = Ticket::decode(code)?;
        start(o, t.topic, Some(t.leader)).await
    }

    /// The team code to give to the other members (the same for everyone).
    pub fn code(&self) -> &str {
        &self.inner.code
    }
    pub fn is_leader(&self) -> bool {
        self.inner.me == self.inner.leader
    }
    pub fn id(&self) -> String {
        self.inner.me.fmt_short().to_string()
    }
    pub fn set_name(&self, n: &str) {
        *self.inner.name.lock().unwrap() = n.chars().take(40).collect();
    }
    /// Our search speed in frames/s (shown to the others).
    pub fn set_fps(&self, f: f32) {
        *self.inner.fps.lock().unwrap() = f;
    }
    /// The host's check for received runs (without one they are held unverified: don't).
    pub fn set_verifier(&self, v: Verifier) {
        self.inner.state.lock().unwrap().verifier = Some(v);
    }

    fn send(&self, m: Msg) {
        match seal(&self.inner.key, m) {
            Ok(b) => {
                let _ = self.inner.out.send(b);
            }
            Err(e) => self.note(format!("not sent: {e:#}")),
        }
    }

    fn note(&self, s: String) {
        let mut st = self.inner.state.lock().unwrap();
        st.log.push_back(s);
        while st.log.len() > 200 {
            st.log.pop_front();
        }
        st.log_gen += 1;
    }

    /// Leader: announces the template (the node's get_block_template JSON). Same blob again: only re-announced.
    pub fn publish_template(&self, json: &str) -> Result<()> {
        anyhow::ensure!(self.is_leader(), "only the leader announces templates");
        self.adopt_template(json.to_string());
        let m = Msg::Template { json: deflate(json.as_bytes()) };
        self.send(m);
        Ok(())
    }

    fn adopt_template(&self, json: String) {
        let tid = template_id(&json);
        let mut st = self.inner.state.lock().unwrap();
        let new = st.template.as_ref().map(|t| t.0) != Some(tid);
        if new {
            st.steps.clear();
            st.step_order.clear();
            st.step_log.clear();
            st.best.clear();
            st.run_gen += 1;
            st.template_gen += 1;
        }
        // The same blob may carry a fresher timestamp: keep the newest JSON for the id.
        st.template = Some((tid, json));
        drop(st);
        if new {
            self.inner.cv.notify_all();
        }
    }

    /// The current template: (id, JSON, generation).
    pub fn template(&self) -> Option<(u64, String, u64)> {
        let st = self.inner.state.lock().unwrap();
        st.template.as_ref().map(|t| (t.0, t.1.clone(), st.template_gen))
    }

    /// Blocks until a template newer than `gen` exists (or `timeout`).
    pub fn wait_template(&self, since: u64, timeout: std::time::Duration) -> Option<(u64, String, u64)> {
        let deadline = n0_future::time::Instant::now() + timeout;
        let mut st = self.inner.state.lock().unwrap();
        loop {
            if st.template_gen > since && st.template.is_some() {
                return st.template.as_ref().map(|t| (t.0, t.1.clone(), st.template_gen));
            }
            let now = n0_future::time::Instant::now();
            if st.closed || now >= deadline {
                return None;
            }
            st = self.inner.cv.wait_timeout(st, deadline - now).unwrap().0;
        }
    }

    /// The current template's generation (changes when the leader announces another template).
    pub fn template_gen(&self) -> u64 {
        self.inner.state.lock().unwrap().template_gen
    }

    /// The shared step for `key` (rrt `step_get`), if some member finished it.
    pub fn get_step(&self, key: &str) -> Option<String> {
        let st = self.inner.state.lock().unwrap();
        let tid = st.template.as_ref()?.0;
        st.steps.get(&(tid, key.to_string())).cloned()
    }

    /// Shares a finished step (rrt `step_put`).
    pub fn put_step(&self, key: &str, json: &str) {
        let tid = {
            let mut st = self.inner.state.lock().unwrap();
            let Some(tid) = st.template.as_ref().map(|t| t.0) else { return };
            st.steps.insert((tid, key.to_string()), json.to_string());
            tid
        };
        self.send(Msg::Step { tid, platform: self.inner.platform.clone(), key: key.to_string(), json: deflate(json.as_bytes()) });
    }

    /// Steps other members shared since `since` (a generation from an earlier call; 0: all): (new generation, [(key, json)]).
    pub fn steps_since(&self, since: u64) -> (u64, Vec<(String, String)>) {
        let st = self.inner.state.lock().unwrap();
        (st.step_gen, st.step_log.iter().filter(|(g, _, _)| *g > since).map(|(_, k, j)| (k.clone(), j.clone())).collect())
    }

    /// Shares a run we found (its sealed proof). It also becomes the held best if shorter.
    pub fn put_run(&self, start: &str, frames: u32, proof: &[u8], keys: &[String], starts: &[u32]) {
        let Some((tid, _, _)) = self.template() else { return };
        let r = RunMsg { tid, start: start.into(), frames, proof: proof.to_vec(), keys: keys.to_vec(), starts: starts.to_vec(), from: self.id() };
        {
            let mut st = self.inner.state.lock().unwrap();
            if st.best.get(start).is_some_and(|b| b.frames <= r.frames) {
                return;
            }
            st.best.insert(start.into(), r.clone());
            st.run_gen += 1;
        }
        self.send(Msg::Run { tid, start: start.into(), frames: r.frames, proof: proof.to_vec(), keys: r.keys, starts: r.starts });
    }

    /// The shortest verified run of the current template for the start kind, and the run generation (bumped when
    /// any best run changes, ours included).
    pub fn best_run(&self, start: &str) -> Option<RunMsg> {
        self.inner.state.lock().unwrap().best.get(start).cloned()
    }
    pub fn run_gen(&self) -> u64 {
        self.inner.state.lock().unwrap().run_gen
    }

    /// Members seen lately (us first).
    pub fn members(&self) -> Vec<Member> {
        let st = self.inner.state.lock().unwrap();
        let mut v = vec![Member { id: self.id(), name: self.inner.name.lock().unwrap().clone(), fps: *self.inner.fps.lock().unwrap(), leader: self.is_leader(), me: true, age: 0 }];
        let mut others: Vec<_> = st
            .members
            .iter()
            .filter(|(id, (_, _, at))| **id != self.inner.me && at.elapsed().as_secs() < MEMBER_TTL)
            .map(|(id, (n, f, at))| Member { id: id.fmt_short().to_string(), name: n.clone(), fps: *f, leader: *id == self.inner.leader, me: false, age: at.elapsed().as_secs() })
            .collect();
        others.sort_by(|a, b| a.id.cmp(&b.id));
        v.extend(others);
        v
    }

    /// Number of direct gossip neighbours (0: not connected to anyone yet).
    pub fn neighbors(&self) -> usize {
        self.inner.state.lock().unwrap().neighbors
    }

    /// Log lines since `gen` (returns the new generation).
    pub fn log_since(&self, since: u64) -> (u64, Vec<String>) {
        let st = self.inner.state.lock().unwrap();
        let n = (st.log_gen - since.min(st.log_gen)) as usize;
        (st.log_gen, st.log.iter().skip(st.log.len().saturating_sub(n)).cloned().collect())
    }

    /// Says goodbye and stops.
    pub fn leave(&self) {
        self.send(Msg::Bye);
        // Let the sender task flush before it is dropped.
        let mut st = self.inner.state.lock().unwrap();
        st.closed = true;
        drop(st);
        self.inner.cv.notify_all();
    }

    fn hello(&self, want_state: bool) {
        let name = self.inner.name.lock().unwrap().clone();
        let fps = *self.inner.fps.lock().unwrap();
        self.send(Msg::Hello { proto: PROTO, name, fps, want_state });
    }

    /// The leader (and anyone who holds them) re-sends the state a late joiner needs.
    fn snapshot(&self, with_steps: bool) {
        if !self.is_leader() {
            return;
        }
        let (t, steps, runs) = {
            let st = self.inner.state.lock().unwrap();
            let Some((tid, json)) = st.template.clone() else { return };
            let steps: Vec<_> = st.steps.iter().filter(|((t, _), _)| with_steps && *t == tid).map(|((_, k), j)| (k.clone(), j.clone())).collect();
            ((tid, json), steps, st.best.values().cloned().collect::<Vec<_>>())
        };
        self.send(Msg::Template { json: deflate(t.1.as_bytes()) });
        for (k, j) in steps {
            self.send(Msg::Step { tid: t.0, platform: self.inner.platform.clone(), key: k, json: deflate(j.as_bytes()) });
        }
        for r in runs {
            self.send(Msg::Run { tid: r.tid, start: r.start, frames: r.frames, proof: r.proof, keys: r.keys, starts: r.starts });
        }
    }

    fn on_msg(&self, from: PublicKey, msg: Msg) {
        let id = from.into();
        match msg {
            Msg::Hello { proto, name, fps, want_state } => {
                if proto != PROTO {
                    self.note(format!("{} speaks protocol {proto}, we speak {PROTO}: ignored", from.fmt_short()));
                    return;
                }
                let name: String = name.chars().take(40).collect();
                let new = self.inner.state.lock().unwrap().members.insert(id, (name.clone(), fps, n0_future::time::Instant::now())).is_none();
                if new {
                    self.note(format!("{} joined", if name.is_empty() { from.fmt_short().to_string() } else { name }));
                    // Tell the newcomer we are here too.
                    self.hello(false);
                }
                if want_state || new {
                    self.snapshot(true);
                }
            }
            Msg::Template { json } => {
                if id != self.inner.leader {
                    self.note(format!("template from {} ignored (not the leader)", from.fmt_short()));
                    return;
                }
                if self.is_leader() {
                    return;
                }
                match inflate(&json).ok().and_then(|b| String::from_utf8(b).ok()).filter(|s| serde_json::from_str::<serde_json::Value>(s).is_ok()) {
                    Some(s) => {
                        let old = self.template().map(|t| t.0);
                        let tid = template_id(&s);
                        self.adopt_template(s);
                        if old != Some(tid) {
                            self.note(format!("new template {tid:016x} from the leader"));
                        }
                    }
                    None => self.note("damaged template ignored".into()),
                }
            }
            Msg::Step { tid, platform, key, json } => {
                if key.len() > 4096 || platform != self.inner.platform {
                    return;
                }
                let Some(s) = inflate(&json).ok().and_then(|b| String::from_utf8(b).ok()) else { return };
                let mut st = self.inner.state.lock().unwrap();
                if st.template.as_ref().map(|t| t.0) != Some(tid) {
                    return;
                }
                if st.steps.insert((tid, key.clone()), s.clone()).is_none() {
                    st.step_gen += 1;
                    let g = st.step_gen;
                    st.step_log.push((g, key.clone(), s));
                    st.step_order.push((tid, key.clone()));
                    while st.step_order.len() > MAX_STEPS {
                        let old = st.step_order.remove(0);
                        st.steps.remove(&old);
                    }
                    drop(st);
                    self.note(format!("got a shared step from {}", from.fmt_short()));
                }
            }
            Msg::Run { tid, start, frames, proof, keys, starts } => {
                if proof.is_empty() || frames == 0 || keys.len() > 4096 || starts.len() > 4096 || start.len() > 16 {
                    return;
                }
                let (cur, ver, unv) = {
                    let st = self.inner.state.lock().unwrap();
                    if st.template.as_ref().map(|t| t.0) != Some(tid) {
                        return;
                    }
                    if st.best.get(&start).is_some_and(|b| b.frames <= frames) {
                        return;
                    }
                    (st.template.as_ref().unwrap().1.clone(), st.verifier.clone(), st.unverified_ok)
                };
                let r = RunMsg { tid, start: start.clone(), frames, proof, keys, starts, from: from.fmt_short().to_string() };
                // Replay it before holding it: a wrong or malicious run is dropped.
                let ok = match ver {
                    Some(v) => v(&cur, &r),
                    None => unv,
                };
                if !ok {
                    self.note(format!("run of {frames} frames from {} did not verify (or no verifier): dropped", from.fmt_short()));
                    return;
                }
                let mut st = self.inner.state.lock().unwrap();
                if st.template.as_ref().map(|t| t.0) == Some(tid) && !st.best.get(&start).is_some_and(|b| b.frames <= frames) {
                    st.best.insert(start, r);
                    st.run_gen += 1;
                    drop(st);
                    self.note(format!("verified run of {frames} frames from {}", from.fmt_short()));
                }
            }
            Msg::Bye => {
                self.inner.state.lock().unwrap().members.remove(&id);
                self.note(format!("{} left", from.fmt_short()));
            }
        }
    }
}

async fn start(o: Opts, topic: [u8; 32], leader: Option<EndpointAddr>) -> Result<Group> {
    let key = SecretKey::generate();
    let me = key.public();
    let endpoint = Endpoint::builder(presets::N0).secret_key(key.clone()).relay_mode(relay_mode(&o)?).bind().await.map_err(|e| anyhow::anyhow!("iroh endpoint: {e}"))?;
    let gossip = Gossip::builder().max_message_size(MAX_MSG + 4096).spawn(endpoint.clone());
    let router = iroh::protocol::Router::builder(endpoint.clone()).accept(ALPN, gossip.clone()).spawn();
    let is_leader = leader.is_none();
    if is_leader || o.relay.as_deref() != Some("none") {
        // A home relay (or none) before the ticket is made: the ticket carries it.
        let _ = n0_future::time::timeout(std::time::Duration::from_secs(8), endpoint.online()).await;
    }
    let leader_addr = leader.clone().unwrap_or_else(|| endpoint.addr());
    let ticket = Ticket { proto: PROTO, topic, leader: leader_addr.clone() };
    let code = ticket.encode();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Bytes>();
    let g = Group {
        inner: Arc::new(Inner { me: me.into(), key: key.clone(), leader: leader_addr.id, name: Mutex::new(o.name.chars().take(40).collect()), fps: Mutex::new(0.0), code, platform: if o.platform.is_empty() { "native".into() } else { o.platform.clone() }, state: Mutex::new(State { unverified_ok: o.unverified_ok, ..State::default() }), cv: Condvar::new(), out: tx }),
    };
    if let Some(l) = leader {
        // Dial the leader: its address (with relay) from the code.
        let mem = iroh::address_lookup::memory::MemoryLookup::new();
        mem.add_endpoint_info(l.clone());
        endpoint.address_lookup()?.add(mem);
    }
    let boot: Vec<EndpointId> = if is_leader { vec![] } else { vec![leader_addr.id] };
    let topic = TopicId::from_bytes(topic);
    let (sender, mut receiver) = gossip.subscribe(topic, boot).await?.split();
    g.note(format!("{}: endpoint {} up", if is_leader { "group created" } else { "joining" }, g.id()));
    // Outgoing.
    let gs = g.clone();
    n0_future::task::spawn(async move {
        let _keep = (router, endpoint);
        while let Some(b) = rx.recv().await {
            if let Err(e) = sender.broadcast(b).await {
                gs.note(format!("send failed: {e}"));
            }
            if gs.inner.state.lock().unwrap().closed && rx.is_empty() {
                // Let the goodbye leave before the endpoint goes.
                n0_future::time::sleep(std::time::Duration::from_millis(700)).await;
                break;
            }
        }
    });
    // Incoming.
    let gr = g.clone();
    n0_future::task::spawn(async move {
        while let Some(ev) = receiver.next().await {
            match ev {
                Ok(Event::Received(m)) => match open(&m.content) {
                    Ok((from, msg)) => gr.on_msg(from, msg),
                    Err(e) => gr.note(format!("bad message dropped: {e:#}")),
                },
                Ok(Event::NeighborUp(id)) => {
                    gr.inner.state.lock().unwrap().neighbors += 1;
                    gr.note(format!("connected to {}", id.fmt_short()));
                    gr.hello(!gr.is_leader());
                }
                Ok(Event::NeighborDown(id)) => {
                    let mut st = gr.inner.state.lock().unwrap();
                    st.neighbors = st.neighbors.saturating_sub(1);
                    drop(st);
                    gr.note(format!("lost {}", id.fmt_short()));
                }
                Ok(_) => {}
                Err(e) => {
                    gr.note(format!("gossip ended: {e}"));
                    break;
                }
            }
        }
    });
    // Heartbeat: presence, the leader's template, and a state request until a template is known.
    let gh = g.clone();
    n0_future::task::spawn(async move {
        loop {
            let (closed, have) = {
                let st = gh.inner.state.lock().unwrap();
                (st.closed, st.template.is_some())
            };
            if closed {
                break;
            }
            gh.hello(!gh.is_leader() && !have);
            if gh.is_leader() {
                // The template and the best runs again (a member that missed them, or whose check was not ready); the steps only on request.
                gh.snapshot(false);
            }
            n0_future::time::sleep(std::time::Duration::from_secs(if have { BEAT } else { 3 })).await;
        }
    });
    Ok(g)
}

/// A group on its own threads, for the native miner (the async [`Group::create`] / [`Group::join`] need a runtime).
#[cfg(not(target_arch = "wasm32"))]
pub mod blocking {
    use super::*;

    fn runtime() -> &'static tokio::runtime::Runtime {
        static RT: std::sync::OnceLock<tokio::runtime::Runtime> = std::sync::OnceLock::new();
        RT.get_or_init(|| tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().thread_name("group").build().expect("tokio runtime"))
    }

    pub fn create(o: Opts) -> Result<Group> {
        runtime().block_on(Group::create(o))
    }
    pub fn join(code: &str, o: Opts) -> Result<Group> {
        runtime().block_on(Group::join(code, o))
    }
}
