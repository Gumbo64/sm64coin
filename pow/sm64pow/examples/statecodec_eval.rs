//! Measures codecs on statedump files: statecodec_eval DUMP.bin WINDOW [simulate_pokes]
use sm64pow::game::Features;
use sm64pow::proof;
use sm64pow::statecodec::Pad as GamePad; // the codec crate's pad (these tools only code frames)
use sm64pow::statecodec::*;
use std::time::Instant;

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

fn cold() -> Model {
    let mut m = Model::new();
    if let Ok(v) = std::env::var("SC_LR") { m.lr = v.parse().unwrap(); }
    if let Ok(v) = std::env::var("SC_LIM") { m.limit = v.parse().unwrap(); }
    m
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

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let window: usize = a[2].parse().unwrap();
    let sim = a.get(3).map(|x| x != "-").unwrap_or(false);
    let prior = a.get(4).map(|p| {
        let mut runs: Vec<Vec<Frame>> = p.split(',').flat_map(|q| load(q)).map(|r| r.1).collect();
        for r in runs.iter_mut() { simulate(r); }
        let lim: u32 = a.get(5).map(|x| x.parse().unwrap()).unwrap_or(1000);
        train(&runs, lim)
    });
    let n0: u16 = a.get(6).map(|x| x.parse().unwrap()).unwrap_or(8);
    let lim2: u32 = a.get(7).map(|x| x.parse().unwrap()).unwrap_or(60);
    let mut tot_prior = 0usize;
    let mut tot = [0usize; 6];
    let mut frames_total = 0usize;
    let (mut t_enc, mut t_dec) = (0f64, 0f64);
    for (_name, run) in load(&a[1]) {
        let mut run = run;
        if sim { simulate(&mut run); }
        for w in run.chunks(window) {
            if w.len() < window.min(500) { continue; }
            let pads: Vec<GamePad> = w.iter().filter(|f| !f.poked).map(|f| f.pad).collect();
            frames_total += w.len();
            tot[0] += 4 * pads.len();
            tot[1] += proof::encode_with(&proof::RunLength, &pads.iter().map(|p| sm64pow::game::from_pad(*p)).collect::<Vec<_>>()).len();
            tot[2] += proof::encode_with(&proof::Xz, &pads.iter().map(|p| sm64pow::game::from_pad(*p)).collect::<Vec<_>>()).len();
            // planar + per-axis stick deltas, xz
            let mut pl = vec![];
            pl.extend(pads.iter().flat_map(|p| p.button.to_le_bytes()));
            let (mut px, mut py) = (0i8, 0i8);
            let mut dx = vec![]; let mut dy = vec![];
            for p in &pads { dx.push(p.stick_x.wrapping_sub(px) as u8); dy.push(p.stick_y.wrapping_sub(py) as u8); px = p.stick_x; py = p.stick_y; }
            pl.extend(dx); pl.extend(dy);
            tot[3] += xz_len(&pl);
            let t = Instant::now();
            let enc = encode_frames_with(cold(), w);
            t_enc += t.elapsed().as_secs_f64();
            tot[4] += enc.len();
            let t = Instant::now();
            let dec = decode_frames_with(cold(), &enc, w);
            t_dec += t.elapsed().as_secs_f64();
            assert_eq!(dec, pads, "round trip");
            tot[5] += 1;
            if let Some(pm) = &prior {
                let enc = encode_frames_with(pm.restart(n0, lim2), w);
                assert_eq!(decode_frames_with(pm.restart(n0, lim2), &enc, w), pads);
                tot_prior += enc.len();
            }
        }
    }
    let bpf = |b: usize| b as f64 * 8.0 / frames_total as f64;
    println!("frames {frames_total} windows {} (window {window})", tot[5]);
    println!("bits/frame: raw {:.3} rle {:.3} xz {:.3} planar+delta xz {:.3} | statectx {:.3}", bpf(tot[0]), bpf(tot[1]), bpf(tot[2]), bpf(tot[3]), bpf(tot[4]));
    if prior.is_some() { println!("with trained prior: {:.3}", bpf(tot_prior)); }
    println!("encode {:.2} us/frame, decode {:.2} us/frame", t_enc * 1e6 / frames_total as f64, t_dec * 1e6 / frames_total as f64);
}
