//! statemix_eval WINDOW DUMP.bin[:sim][:maxruns] ... : bits/frame of xz, codec 3 and the statemix model
//! (cost metered; round trip checked with SC_RT=1). Cfg overrides in SC_CFG="key=val,..".
use sm64pow::game::Features;
use sm64pow::proof::{self, Codec};
use sm64pow::statecodec::Pad as GamePad; // the codec crate's pad (these tools only code frames)
use sm64pow::statecodec::{self, CostMeter, Frame};
use sm64pow::statemix::{Cfg, Model};
use std::sync::Mutex;

fn load(path: &str) -> Vec<(String, Vec<Frame>)> {
    let b = std::fs::read(path).unwrap();
    let mut i = 0;
    let rd = |i: &mut usize| {
        let v = u32::from_le_bytes(b[*i..*i + 4].try_into().unwrap());
        *i += 4;
        v
    };
    let mut runs = vec![];
    while i < b.len() {
        let n = rd(&mut i) as usize;
        let name = String::from_utf8_lossy(&b[i..i + n]).to_string();
        i += n;
        let rows = rd(&mut i) as usize;
        let mut fr = vec![];
        for _ in 0..rows {
            let mut r = [0i32; 25];
            for v in r.iter_mut() {
                *v = rd(&mut i) as i32;
            }
            let f = Features {
                action: r[0] as u32, prev_action: r[1] as u32, flags: r[2] as u32, input: r[3] as u16, action_state: r[4] as u16,
                action_timer: r[5] as u16, intended_yaw: r[6] as u16, intended_mag: r[7], face_yaw: r[8] as u16, cam_yaw: r[9] as u16,
                pos: [r[10], r[11], r[12]], fwd: r[13], vel: [r[14], r[15], r[16]], health: r[17] as u16, level: r[18] as u16,
                area: r[19] as u16, timer: r[20] as u32,
            };
            fr.push(Frame { features: f, pad: GamePad::new(r[21] as u16, r[22] as i8, r[23] as i8), poked: r[24] != 0 });
        }
        runs.push((name, fr));
    }
    runs
}

struct NullIo;
impl sm64pow::statecodec::BitIo for NullIo {
    fn bit(&mut self, b: bool, _: u32) -> bool { b }
    fn bit16(&mut self, b: bool, _: u32) -> bool { b }
}

fn xz_len(raw: &[u8]) -> usize {
    use std::io::Write;
    let mut e = xz2::write::XzEncoder::new(Vec::new(), 9 | 0x8000_0000);
    e.write_all(raw).unwrap();
    e.finish().unwrap().len()
}

fn simulate(run: &mut [Frame]) {
    let mut s = 0x9E3779B97F4A7C15u64 ^ run.len() as u64;
    let mut burst = 0;
    for f in run.iter_mut() {
        s ^= s << 13; s ^= s >> 7; s ^= s << 17;
        if burst > 0 { burst -= 1; f.poked = true; } else if (s >> 40) % 1000 < 45 { f.poked = true; burst = 1; }
    }
}

fn best_of(window: usize, specs: &[String]) {
    let cfgs: Vec<Cfg> = std::env::var("SC_CFGS").unwrap().split(';').map(Cfg::parse).collect();
    for spec in specs {
        let parts: Vec<&str> = spec.split(':').collect();
        let sim = parts.get(1).map(|x| *x == "sim").unwrap_or(false);
        let maxruns: usize = parts.get(2).map(|x| x.parse().unwrap()).unwrap_or(usize::MAX);
        let (mut frames, mut best) = (0usize, 0f64);
        let mut each = vec![0f64; cfgs.len()];
        for (_n, mut run) in load(parts[0]).into_iter().take(maxruns) {
            if sim { simulate(&mut run); }
            for w in run.chunks(window) {
                if w.len() < window.min(500) { continue; }
                frames += w.len();
                let mut mn = f64::MAX;
                for (k, c) in cfgs.iter().enumerate() {
                    let mut m = Model::new(c.clone());
                    let mut io = CostMeter::default();
                    for f in w { if f.poked { m.poked(); } else { m.code_pad(&mut io, &f.features, f.pad); } }
                    each[k] += io.bits;
                    mn = mn.min(io.bits);
                }
                best += mn;
            }
        }
        println!("{}: each {:?} best-of {:.3} bits/frame", parts[0].rsplit('/').next().unwrap(), each.iter().map(|b| format!("{:.3}", b / frames as f64)).collect::<Vec<_>>(), best / frames as f64);
    }
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let window: usize = a[1].parse().unwrap();
    if let Ok(p) = std::env::var("SC_PROFILE") { profile(&p, window, std::env::var("SC_SIM").is_ok()); return; }
    if std::env::var("SC_CFGS").is_ok() { best_of(window, &a[2..]); return; }
    let cfg = Cfg::parse(&std::env::var("SC_CFG").unwrap_or_default());
    let rt = std::env::var("SC_RT").is_ok();
    // SC_PRIOR=dump[:maxruns],... trains a prior (pokes simulated); SC_PW=1 keeps weights, SC_PN0 counters' confidence.
    let prior: Option<Model> = std::env::var("SC_PRIOR").ok().map(|p| {
        let mut m = Model::new(cfg.clone());
        for spec in p.split(',') {
            let (path, mx) = spec.split_once(':').map(|(a, b)| (a, b.parse::<usize>().unwrap())).unwrap_or((spec, usize::MAX));
            for (_n, mut run) in load(path).into_iter().take(mx) {
                simulate(&mut run);
                m = m.restart(true, 255);
                let mut io = CostMeter::default();
                for f in &run { if f.poked { m.poked(); } else { m.code_pad(&mut io, &f.features, f.pad); } }
            }
        }
        m
    });
    let pw = std::env::var("SC_PW").map(|v| v != "0").unwrap_or(true);
    let pn0: u16 = std::env::var("SC_PN0").map(|v| v.parse().unwrap()).unwrap_or(0);
    let newm = |c: &Cfg| -> Model { match &prior { Some(p) => p.restart(pw, pn0), None => Model::new(c.clone()) } };
    for spec in &a[2..] {
        let parts: Vec<&str> = spec.split(':').collect();
        let sim = parts.get(1).map(|x| *x == "sim").unwrap_or(false);
        let maxruns: usize = parts.get(2).map(|x| x.parse().unwrap()).unwrap_or(usize::MAX);
        let mut wins: Vec<Vec<Frame>> = vec![];
        for (_n, mut run) in load(parts[0]).into_iter().take(maxruns) {
            if sim { simulate(&mut run); }
            for w in run.chunks(window) {
                if w.len() >= window.min(500) { wins.push(w.to_vec()); }
            }
        }
        let tot = Mutex::new([0f64; 5]);
        let next = Mutex::new(0usize);
        let t0 = std::time::Instant::now();
        std::thread::scope(|s| {
            for _ in 0..std::env::var("SC_THREADS").map(|v| v.parse().unwrap()).unwrap_or(4) {
                s.spawn(|| loop {
                    let i = { let mut n = next.lock().unwrap(); let i = *n; *n += 1; i };
                    if i >= wins.len() { break; }
                    let w = &wins[i];
                    let pads: Vec<GamePad> = w.iter().filter(|f| !f.poked).map(|f| f.pad).collect();
                    let xz = xz_len(&pads.iter().flat_map(|p| { let mut v = p.button.to_le_bytes().to_vec(); v.push(p.stick_x as u8); v.push(p.stick_y as u8); v }).collect::<Vec<u8>>());
                    let xz2 = proof::encode_with(&proof::Xz, &pads.iter().map(|p| sm64pow::game::from_pad(*p)).collect::<Vec<_>>()).len();
                    let _ = xz;
                    let c3 = statecodec::encode_frames(w).len();
                    let t_start = std::time::Instant::now();
                    let mut m = newm(&cfg);
                    {
                        let mut io = NullIo;
                        for f in w { if f.poked { m.poked(); } else { m.code_pad(&mut io, &f.features, f.pad); } }
                    }
                    let tm = t_start.elapsed().as_secs_f64();
                    let mut m = newm(&cfg);
                    let mut io = CostMeter::default();
                    for f in w {
                        if f.poked { m.poked(); } else { m.code_pad(&mut io, &f.features, f.pad); }
                    }
                    let bytes = (io.bits / 8.0).ceil();
                    if rt {
                        let mut m = Model::new(cfg.clone());
                        let mut e = statecodec::RangeEncoder::default();
                        for f in w { if f.poked { m.poked(); } else { m.code_pad(&mut e, &f.features, f.pad); } }
                        let enc = e.finish();
                        let mut m = Model::new(cfg.clone());
                        let mut d = statecodec::RangeDecoder::new(&enc);
                        let mut out = vec![];
                        for f in w { if f.poked { m.poked(); } else { out.push(m.code_pad(&mut d, &f.features, GamePad::default())); } }
                        assert_eq!(out, pads, "round trip");
                    }
                    let mut t = tot.lock().unwrap();
                    t[4] += tm;
                    t[0] += w.len() as f64; t[1] += xz2 as f64; t[2] += c3 as f64; t[3] += bytes;
                });
            }
        });
        let t = tot.lock().unwrap();
        let b = |x: f64| x * 8.0 / t[0];
        println!("{:<28} frames {:>8} xz {:.3} c3 {:.3} new {:.3} | vs xz {:+.1}% vs c3 {:+.1}%  ({:.1}s) {:.2} us/frame", parts[0].rsplit('/').next().unwrap(), t[0], b(t[1]), b(t[2]), b(t[3]), (t[3] / t[1] - 1.0) * 100.0, (t[3] / t[2] - 1.0) * 100.0, t0.elapsed().as_secs_f64(), t[4] * 1e6 / t[0]);
    }
}

/// Cost per node group of one dump (SC_PROFILE=path).
#[allow(dead_code)]
pub fn profile(path: &str, window: usize, sim: bool) {
    let mut cfg = Cfg::parse(&std::env::var("SC_CFG").unwrap_or_default());
    cfg.trace = true;
    let mut cost = std::collections::BTreeMap::<&str, (f64, u64)>::new();
    let mut frames = 0usize;
    for (_n, mut run) in load(path) {
        if sim { simulate(&mut run); }
        for w in run.chunks(window) {
            let mut m = Model::new(cfg.clone());
            let mut io = CostMeter::default();
            for f in w { if f.poked { m.poked(); } else { m.code_pad(&mut io, &f.features, f.pad); } }
            frames += w.len();
            for &(n, p, b) in &m.probe {
                let c = -((if b { p } else { 65536 - p }) as f64 / 65536.0).log2();
                let g = match n { 0 => "0 same", 1 => "1 bchg", 2..=4 => "2 btn-mru", 8..=39 => "3 btn-explicit", 40 => "4 schg", 41 => "5 stick=pred", 45 => "5b stick=sweep", 48 => "7a x=pred", 49 => "7b x near", 50..=66 => "7c x near sign/dist", 67..=127 => "7d x far", 128 | 208 => "8a y=pred", 129 | 209 => "8b y near", 130..=146 | 210..=226 => "8c y near sign/dist", 42..=44 => "6 stick-mru", 48..=127 => "7 x-axis", _ => "8 y-axis" };
                let e = cost.entry(g).or_default();
                e.0 += c; e.1 += 1;
            }
        }
    }
    for (g, (c, n)) in &cost { println!("{g:<16} decisions {n:>9} bits {c:>10.0}  = {:.3} bits/frame", c / frames as f64); }
}
