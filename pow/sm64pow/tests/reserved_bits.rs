//! Does the game read the controller's two reserved button bits (0x0080 and 0x0040; both set is a
//! console reset in Mupen64Plus)? Replays real chain runs with the bits forced on, forced off, and as
//! played, and compares the game after every frame: full memory, and the features/progress the
//! game logic exposes. The consequence (v0.3.1): miners never seal such a pad (normalise.rs `without_reserved_bits`) and the
//! chain export clears the bits in m64 output.

use sm64pow::chain::{Parent, played_block};
use sm64pow::game::{read_features, read_progress};
use sm64pow::sm64_binds::GamePad;

const RESERVED: u16 = 0x00C0;

fn hex(s: &str) -> Vec<u8> {
    (0..s.len() / 2).map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap()).collect()
}

fn mem_hash(g: &sm64pow::sm64_binds::SM64Game) -> u64 {
    g.with_memory(|m| m.iter().fold(0xcbf29ce484222325u64, |h, &b| (h ^ b as u64).wrapping_mul(0x100000001b3)))
}

/// (memory hash, logic key) after every frame of `pads` from power-on.
fn trace(pads: &[GamePad]) -> Vec<(u64, String)> {
    let mut g = sm64pow::generator().unwrap().create_game().unwrap();
    pads.iter()
        .map(|p| {
            g.step_game(*p).unwrap();
            (mem_hash(&g), format!("{:?} {:?}", read_features(&g).unwrap(), read_progress(&g).unwrap().points()))
        })
        .collect()
}

#[test]
fn the_reserved_button_bits_do_not_change_the_game() {
    let rom = std::env::var("SM64COIN_ROM").unwrap_or_else(|_| "../../baserom.us.z64".into());
    let Ok(b) = std::fs::read(&rom) else {
        eprintln!("skipping: no ROM at {rom} (set SM64COIN_ROM)");
        return;
    };
    let _ = sm64pow::init(b);
    let l = include_str!("fixtures/mainnet_v02.jsonl").lines().next().unwrap();
    let j: serde_json::Value = serde_json::from_str(l).unwrap();
    let id: [u8; 32] = hex(j["id"].as_str().unwrap()).try_into().unwrap();
    let seed = sm64pow::Seed::from_hex(j["seed"].as_str().unwrap()).unwrap();
    let blob = hex(j["proof"].as_str().unwrap());
    let mut played = played_block(&id, Parent::Genesis, &seed, &blob).unwrap().unwrap();
    played.truncate(1500); // (a prefix is a run to the game; the check is per frame)
    let with = |f: &dyn Fn(u16) -> u16| -> Vec<GamePad> { played.iter().map(|p| GamePad::new(f(p.button), p.stick_x, p.stick_y)).collect() };
    let natural = played.iter().filter(|p| p.button & RESERVED == RESERVED).count();
    let (base, on, off) = (trace(&played), trace(&with(&|b| b | RESERVED)), trace(&with(&|b| b & !RESERVED)));
    let diff_mem = |a: &[(u64, String)], b: &[(u64, String)]| a.iter().zip(b).filter(|(x, y)| x.0 != y.0).count();
    let diff_logic = |a: &[(u64, String)], b: &[(u64, String)]| a.iter().zip(b).filter(|(x, y)| x.1 != y.1).count();
    eprintln!("{} frames, {natural} played with both reserved bits set; frames whose memory differs from the played run: forced on {}, forced off {}; whose game logic differs: on {}, off {}", played.len(), diff_mem(&base, &on), diff_mem(&base, &off), diff_logic(&base, &on), diff_logic(&base, &off));
    assert_eq!(diff_logic(&base, &on), 0, "the game reads the reserved bits");
    assert_eq!(diff_logic(&base, &off), 0, "the game reads the reserved bits");
}

/// The miner's guard: a run with both bits set on some pads is sealed with them cleared, the game logic
/// unchanged (per frame, the perturbation included).
#[cfg(feature = "normalise")]
#[test]
fn the_miner_clears_pads_with_both_reserved_bits() {
    let rom = std::env::var("SM64COIN_ROM").unwrap_or_else(|_| "../../baserom.us.z64".into());
    let Ok(b) = std::fs::read(&rom) else { return };
    let _ = sm64pow::init(b);
    let l = include_str!("fixtures/mainnet_v02.jsonl").lines().next().unwrap();
    let j: serde_json::Value = serde_json::from_str(l).unwrap();
    let id: [u8; 32] = hex(j["id"].as_str().unwrap()).try_into().unwrap();
    let seed = sm64pow::Seed::from_hex(j["seed"].as_str().unwrap()).unwrap();
    let blob = hex(j["proof"].as_str().unwrap());
    let mut played = played_block(&id, Parent::Genesis, &seed, &blob).unwrap().unwrap();
    played.truncate(600);
    let mut dirty = played.clone();
    for (i, p) in dirty.iter_mut().enumerate() {
        if i % 7 == 3 {
            *p = GamePad::new(p.button | RESERVED, p.stick_x, p.stick_y);
        }
    }
    let p = sm64pow::params(1);
    let clean = sm64pow::normalise::without_reserved_bits(sm64pow::proof::Start::PowerOn, None, &seed, &p, &dirty);
    assert!(clean.iter().all(|q| q.button & RESERVED != RESERVED));
    assert_eq!(clean.len(), dirty.len());
    // A run without any is returned as it is.
    assert_eq!(sm64pow::normalise::without_reserved_bits(sm64pow::proof::Start::PowerOn, None, &seed, &p, &played), played);
}
