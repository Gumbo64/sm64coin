//! The v0.3.1 final tie-break through the daemon's C ABI (the code path the node runs): two siblings
//! of different keys with the same frames and the same canonical proof size (the same real proof
//! blob, a v0.2 block of the fixture) differ only in their template seeds. Whatever order they
//! arrive in, every node keeps the one whose cn_fast_hash(seed) is lower; the signature (which an
//! owner could grind by re-signing) is not an input.
use sm64pow::ffi::{sm64pow_displaces, sm64pow_note_eligible};
use sm64pow::sign::cn_fast_hash;

fn hex(s: &str) -> Vec<u8> {
    (0..s.len() / 2).map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap()).collect()
}

fn note(id: u8, parent: u8, seed: &[u8; 32], sig: &[u8; 64], blob: &[u8], frames: u32, ts: u64) {
    sm64pow_note_eligible([id; 32].as_ptr(), [parent; 32].as_ptr(), seed.as_ptr(), frames, ts, sig.as_ptr(), blob.as_ptr(), blob.len());
}

#[test]
fn exact_ties_go_to_the_lower_seed_hash_in_every_arrival_order_whatever_the_signatures() {
    let l = include_str!("fixtures/mainnet_v02.jsonl").lines().next().unwrap();
    let blob = hex(serde_json::from_str::<serde_json::Value>(l).unwrap()["proof"].as_str().unwrap());
    let frames = sm64pow::proof::split_blob(&blob).unwrap().0;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    // Many seed pairs: the winner is always the lower hash.
    for k in 0..24u8 {
        let (sa, sb) = ([k; 32], [k.wrapping_add(100); 32]);
        let a_wins = cn_fast_hash(&sa) < cn_fast_hash(&sb);
        // Two arrival orders, each with its own parent so the node's state is fresh; and two
        // signature choices (unrelated bytes) that must not matter.
        for (order, sigs) in [(0u8, ([1u8; 64], [2u8; 64])), (1, ([200u8; 64], [3u8; 64])), (2, ([0u8; 64], [255u8; 64])), (3, ([9u8; 64], [9u8; 64]))] {
            let parent = 1 + k * 4 + order % 4;
            let (ida, idb) = (10 + k * 4 + order % 4, 130 + k * 4 + order % 4);
            let a_first = order % 2 == 0;
            if a_first {
                note(ida, parent, &sa, &sigs.0, &blob, frames, now);
                note(idb, parent, &sb, &sigs.1, &blob, frames, now + 1);
            } else {
                note(idb, parent, &sb, &sigs.1, &blob, frames, now);
                note(ida, parent, &sa, &sigs.0, &blob, frames, now + 1);
            }
            let (tip, alt) = if a_first { (ida, idb) } else { (idb, ida) };
            let displaced = sm64pow_displaces([tip; 32].as_ptr(), [alt; 32].as_ptr()) == 1;
            let winner_is_a = if displaced { !a_first } else { a_first };
            assert_eq!(winner_is_a, a_wins, "seeds {k}: order {order}: the lower seed hash must win");
        }
    }
}
