//! Dumps per-frame (state features, pad) rows of runs, for training and
//! analysing state-aware proof codecs.
//!
//! statedump OUT.bin m64 PATH...        raw/M64 power-on movies (unperturbed)
//! statedump OUT.bin run RUN.json...    proof runs (see tools/statecodec/README)
//!
//! OUT.bin: a sequence of runs, each `u32 name_len, name, u32 rows` then rows
//! of 25 little-endian i32: the Features fields, buttons, stick x, stick y,
//! poked.
use sm64pow::game::{read_features, Features};
use sm64pow::sm64_binds::GamePad;
use std::io::Write;

fn parse_m64(b: &[u8]) -> Vec<GamePad> {
    let body = if b.len() >= 0x400 && &b[..4] == b"M64\x1a" { &b[0x400..] } else { b };
    // The M64 header form stores buttons big-endian too.
    body.chunks_exact(4).map(|c| GamePad::new(u16::from_be_bytes([c[0], c[1]]), c[2] as i8, c[3] as i8)).collect()
}

pub fn row(f: &Features, p: &GamePad, poked: bool) -> [i32; 25] {
    [
        f.action as i32, f.prev_action as i32, f.flags as i32, f.input as i32, f.action_state as i32, f.action_timer as i32,
        f.intended_yaw as i32, f.intended_mag, f.face_yaw as i32, f.cam_yaw as i32, f.pos[0], f.pos[1], f.pos[2], f.fwd,
        f.vel[0], f.vel[1], f.vel[2], f.health as i32, f.level as i32, f.area as i32, f.timer as i32,
        p.button as i32, p.stick_x as i32, p.stick_y as i32, poked as i32,
    ]
}

fn write_run(out: &mut impl Write, name: &str, rows: &[[i32; 25]]) {
    out.write_all(&(name.len() as u32).to_le_bytes()).unwrap();
    out.write_all(name.as_bytes()).unwrap();
    out.write_all(&(rows.len() as u32).to_le_bytes()).unwrap();
    for r in rows {
        for v in r {
            out.write_all(&v.to_le_bytes()).unwrap();
        }
    }
}

fn hex(s: &str) -> Vec<u8> {
    (0..s.len() / 2).map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap()).collect()
}

/// Run files: {"seed_hash", "start": 0|1, "frames", "inputs": hex of raw pads
/// (button LE u16, x, y), "parent_played": same for the cycle up to the parent}.
pub fn run_from_json(j: &serde_json::Value) -> anyhow::Result<sm64pow::statecodec::Run> {
    use sm64pow::proof::{pad_from, Proof, Start};
    let pads = |k: &str| -> Vec<GamePad> { hex(j[k].as_str().unwrap_or("")).chunks_exact(4).map(pad_from).collect() };
    Ok(sm64pow::statecodec::Run {
        seed: sm64pow::Seed::from_hex(j["seed_hash"].as_str().unwrap())?,
        parent_played: pads("parent_played"),
        proof: Proof { frames: j["frames"].as_u64().unwrap() as u32, start: if j["start"].as_u64() == Some(1) { Start::PowerOn } else { Start::Parent }, inputs: pads("inputs") },
    })
}

fn main() -> anyhow::Result<()> {
    let a: Vec<String> = std::env::args().collect();
    sm64pow::init_default()?;
    let mut out = std::io::BufWriter::new(std::fs::File::create(&a[1])?);
    match a[2].as_str() {
        "m64" => {
            for path in &a[3..] {
                let pads = parse_m64(&std::fs::read(path)?);
                let mut g = sm64pow::generator()?.create_game()?;
                let mut rows = Vec::with_capacity(pads.len());
                for p in &pads {
                    rows.push(row(&read_features(&g)?, p, false));
                    g.step_game(*p)?;
                }
                write_run(&mut out, path, &rows);
            }
        }
        "run" => {
            for path in &a[3..] {
                let j: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path)?)?;
                let r = run_from_json(&j)?;
                let rows = r.dump_rows()?;
                write_run(&mut out, path, &rows.iter().map(|(f, p, k)| row(f, p, *k)).collect::<Vec<_>>());
            }
        }
        _ => anyhow::bail!("mode"),
    }
    Ok(())
}
