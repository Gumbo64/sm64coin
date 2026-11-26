//! Port vs Mupen64Plus RAM, poll by poll (diagnostic behind DIVERGENCE.md).
//!   divergence CYCLE.m64 DUMP.bin [pool] [first-only]
//! DUMP.bin is written by tools/mupen64plus/m64_input.c (M64_DUMP=a-b, M64_DUMP_FILE, optional
//! M64_DUMP_NOPOOL): per poll a 16-byte header (poll number), Mario's struct (0xC8), RNG (4), timer (4)
//! and, unless NOPOOL, the 240-object pool, as big-endian N64 bytes. The record of poll p is the port after
//! p - 2 frames (the .mupen64plus.m64 variant has one leading zero poll). Prints each differing word.
use sm64_binds::SM64Game;

const PORT_MARIO: u32 = 0xb0a010;
const PORT_TIMER: u32 = 0xb09e94;
const PORT_RNG: u32 = 0xb05f20;
const PORT_POOL: u32 = 0xb8ca90;
const MARIO_LEN: usize = 0xc8;
const OBJ: usize = 0x260;

fn pb(g: &SM64Game, a: u32, n: usize) -> Vec<u8> {
    let mut b = vec![0u8; n];
    g.read_memory(a, &mut b).unwrap();
    b
}
fn w(b: &[u8], o: usize) -> u32 { u32::from_le_bytes(b[o..o + 4].try_into().unwrap()) }
fn wb(b: &[u8], o: usize) -> u32 { u32::from_be_bytes(b[o..o + 4].try_into().unwrap()) }
fn ptr_like(n: u32) -> bool { (0x80000000..0x80800000).contains(&n) }
/// N64 big-endian word `a` vs the port's little-endian word `b`: the same bytes under any of the
/// layouts a 32-bit word can hold (one s32/f32, two s16, four u8, ...).
fn same(a: u32, b: u32) -> bool {
    let h = ((a & 0x00ff00ff) << 8) | ((a >> 8) & 0x00ff00ff);
    a == b || a.rotate_left(16) == b || a.swap_bytes() == b || h == b
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    sm64pow::init(std::fs::read(std::env::var("SM64COIN_ROM")?)?)?;
    let (_, pads) = sm64coin_export::m64::read(&std::fs::read(&args[1])?)?;
    let dump = std::fs::read(&args[2])?;
    let pool = args.iter().any(|a| a == "pool");
    let first_only = args.iter().any(|a| a == "first-only");
    let rec = 16 + MARIO_LEN + 8 + if pool { 240 * OBJ } else { 0 };
    let mut g = sm64coin_export::export::fresh_game()?;
    let mut done = 0usize;
    let mut bad = 0;
    for r in dump.chunks_exact(rec) {
        let poll = i64::from_le_bytes(r[..8].try_into().unwrap()) as usize;
        let frames = poll - 2;
        while done < frames { g.step_game(pads[done])?; done += 1; }
        let nm = &r[16..16 + MARIO_LEN];
        let pm = pb(&g, PORT_MARIO, MARIO_LEN);
        let mut out = vec![];
        for o in (0..MARIO_LEN).step_by(4) {
            let (a, b) = (wb(nm, o), w(&pm, o));
            if o != 0xac && !same(a, b) && !(ptr_like(a) && b < 0x1000000) {
                out.push(format!("  mario+{o:#x}: n64 {a:#010x} ({}) port {b:#010x} ({})", f32::from_bits(a), f32::from_bits(b)));
            }
        }
        let t1 = wb(r, 16 + MARIO_LEN + 4);
        let t2 = w(&pb(&g, PORT_TIMER, 4), 0);
        let r1 = u16::from_be_bytes([r[16 + MARIO_LEN], r[16 + MARIO_LEN + 1]]);
        let r2 = u16::from_le_bytes(pb(&g, PORT_RNG, 2).try_into().unwrap());
        if t1 != t2 || r1 != r2 { out.push(format!("  timer n64 {t1} port {t2}; rng n64 {r1:#x} port {r2:#x}")); }
        if pool {
            let np = &r[16 + MARIO_LEN + 8..];
            let pp = pb(&g, PORT_POOL, 240 * OBJ);
            let mut shown = 0;
            for s in 0..240 {
                let mut d = vec![];
                for o in (0..OBJ).step_by(4) {
                    let (a, b) = (wb(np, s * OBJ + o), w(&pp, s * OBJ + o));
                    if !(0x54..=0x5c).contains(&o) && !same(a, b) && !(ptr_like(a) && b < 0x1000000) { d.push(format!("+{o:#x}:{a:#x}/{b:#x}")); }
                }
                if !d.is_empty() && shown < 8 { shown += 1; out.push(format!("  obj{s}: {}", d[..d.len().min(12)].join(" "))); }
            }
        }
        if !out.is_empty() {
            bad += 1;
            println!("== poll {poll} (port after {frames} frames): differs");
            for l in out { println!("{l}"); }
            if first_only { break; }
        }
    }
    println!("{} records, {bad} differing", dump.len() / rec);
    Ok(())
}
