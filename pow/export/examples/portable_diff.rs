//! Where does a game loaded from a block's portable start state first differ from the same game loaded
//! from the block's exact (raw) start state?   portable_diff BLOCK_PREFIX  (e.g. out/blocks/h0000011_e43083fa)
//! Steps both with the block's pads and, at the first frame where the GameState, Mario, the RNG or any
//! portable variable differs, prints what differs (variable names, and the differing words with DETAIL=1).
use flate2::read::GzDecoder;
use sm64_binds::portable::Portable;
use std::io::Read;

fn gunzip(p: &str) -> Vec<u8> { let mut v = vec![]; GzDecoder::new(std::fs::File::open(p).unwrap()).read_to_end(&mut v).unwrap(); v }

fn main() -> anyhow::Result<()> {
    let pre = std::env::args().nth(1).expect("block prefix");
    sm64pow::init(std::fs::read(std::env::var("SM64COIN_ROM")?)?)?;
    let raw = sm64_binds::SaveState::from_bytes(gunzip(&format!("{pre}.start.state.gz")))?;
    // FRESH=1: export the portable state from the raw start state with the translator linked here, instead
    // of reading the .start.portable.gz the export wrote (which a newer/older translator made).
    let port = if std::env::var("FRESH").is_ok() {
        let mut g = sm64coin_export::export::fresh_game()?;
        g.load_state(&raw)?;
        g.export_portable().map_err(|e| anyhow::anyhow!("{e}"))?
    } else {
        Portable::from_bytes(&gunzip(&format!("{pre}.start.portable.gz"))).map_err(|e| anyhow::anyhow!("{e}"))?
    };
    let pads = sm64coin_export::m64::frames_from_bytes(&std::fs::read(format!("{pre}.pads"))?)?;
    let mut a = sm64coin_export::export::fresh_game()?; // exact
    a.load_state(&raw)?;
    let mut b = sm64coin_export::export::fresh_game()?; // from the portable state
    b.load_portable(&port).map_err(|e| anyhow::anyhow!("{e}"))?;
    let rd = |g: &sm64_binds::SM64Game, ad: u32, n: usize| { let mut v = vec![0u8; n]; g.read_memory(ad, &mut v).unwrap(); v };
    let var = |g: &sm64_binds::SM64Game| g.export_portable().map_err(|e| anyhow::anyhow!("{e}")).map(|p| p.digest());
    let mut n_diff = 0;
    for f in 0..=pads.len() {
        let (ga, gb) = (a.get_game_state()?, b.get_game_state()?);
        let (ra, rb) = (rd(&a, 0xb05f20, 2), rd(&b, 0xb05f20, 2));
        let key = |g: &sm64_binds::SM64Game| { let m = rd(g, 0xb0a010, 0xc8); [&m[12..16], &m[60..84], &m[168..176]].concat() }; // action, pos, vel, coins/stars/health
        let (ma, mb) = (key(&a), key(&b));
        let (da, db) = (var(&a)?, var(&b)?);
        let names: Vec<_> = (0..da.len().min(db.len())).filter(|&i| da[i] != db[i]).map(Portable::var_name).filter(|n| !n.contains("gMatStack") && !n.contains("gBodyStates")).collect();
        if std::env::var("MEMDIFF").ok().and_then(|v| v.parse::<usize>().ok()) == Some(f) {
            // every differing word of the data/bss (below the heap), named by the headless linker map
            let map = std::fs::read_to_string("/workspaces/sm64-combined/sm64-binds/sm64-port/build/us_web_headless/sm64.map")?;
            let mut syms: Vec<(u32, String)> = map.lines().filter_map(|l| { let c: Vec<&str> = l.split_whitespace().collect(); (c.len() == 4 && c[3].contains(":(.")).then(|| u32::from_str_radix(c[0], 16).ok().map(|a| (a, c[3].rsplit(":(.").next().unwrap().trim_end_matches(')').to_string()))).flatten() }).collect();
            syms.sort();
            let (lo, hi) = (0x7b0000u32, 0xbf9000u32);
            let (ma, mb) = (rd(&a, lo, (hi - lo) as usize), rd(&b, lo, (hi - lo) as usize));
            let mut last = String::new(); let mut n = 0;
            for o in (0..ma.len()).step_by(4) {
                let (x, y) = (u32::from_le_bytes(ma[o..o + 4].try_into().unwrap()), u32::from_le_bytes(mb[o..o + 4].try_into().unwrap()));
                if x != y && !(x > 0xc00000 && y > 0xc00000 && x < 0x4000_0000 && y < 0x4000_0000) {
                    let ad = lo + o as u32; let i = syms.partition_point(|s| s.0 <= ad).saturating_sub(1);
                    let name = format!("{}+{:#x}", syms[i].1, ad - syms[i].0);
                    let base = syms[i].1.clone();
                    if base != last { println!("  mem {ad:#x} {name}: {x:#x}/{y:#x}"); last = base; n += 1; if n > 60 { break; } }
                }
            }
        }
        if ga != gb || ra != rb || ma != mb || !names.is_empty() {
            println!("frame {f}: game state equal {}, rng {:02x?}/{:02x?}, mario equal {}, variables differing: {:?}", ga == gb, ra, rb, ma == mb, &names[..names.len().min(12)]);
            if std::env::var("DETAIL").is_ok() {
                let (pa, pb) = (rd(&a, 0xb8ca90, 240 * 0x260), rd(&b, 0xb8ca90, 240 * 0x260));
                for s in 0..240 {
                    let mut d = vec![];
                    for o in (0..0x260).step_by(4) {
                        let (x, y) = (u32::from_le_bytes(pa[s * 0x260 + o..][..4].try_into().unwrap()), u32::from_le_bytes(pb[s * 0x260 + o..][..4].try_into().unwrap()));
                        if x != y && !(x > 0x100000 && y > 0x100000 && x < 0x4000_0000 && y < 0x4000_0000) { d.push(format!("+{o:#x}:{x:#x}/{y:#x}")); }
                    }
                    if !d.is_empty() { println!("  slot {s}: {}", d[..d.len().min(10)].join(" ")); }
                }
            }
            n_diff += 1;
            if n_diff >= std::env::var("N").ok().and_then(|n| n.parse().ok()).unwrap_or(3) { break; }
        }
        if f < pads.len() { a.step_game(pads[f])?; b.step_game(pads[f])?; }
    }
    if n_diff == 0 { println!("identical for {} frames", pads.len()); }
    Ok(())
}
