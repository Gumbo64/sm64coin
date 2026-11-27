//! The trust anchors compiled into a release. `checkpoints/<network>.json` is a JSON array of
//! [`Checkpoint`]s, oldest first, regenerated at release time from a trusted full node with
//! `cargo run --example checkpoint --features http,tls -- <node url> --depth 36` (the last one
//! should sit at least the reorg cap, 36 blocks, below the tip). The genesis checkpoint is always
//! first, so a client older than every other entry still has an anchor.
use crate::verify::Checkpoint;

pub const MAINNET_JSON: &str = include_str!("../checkpoints/mainnet.json");

fn parse(json: &str) -> Vec<Checkpoint> {
    let list: Vec<Checkpoint> = serde_json::from_str(json).expect("compiled-in checkpoint table parses");
    for c in &list {
        c.validate().expect("compiled-in checkpoint is consistent");
    }
    list
}

/// Every mainnet checkpoint, oldest first.
pub fn mainnet() -> Vec<Checkpoint> {
    parse(MAINNET_JSON)
}

/// The newest mainnet checkpoint: the anchor a client verifies forward from.
pub fn mainnet_latest() -> Checkpoint {
    mainnet().pop().expect("the table is never empty")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_compiled_table_is_sane() {
        let t = mainnet();
        assert_eq!(t[0].height, 0, "genesis first");
        assert!(t.windows(2).all(|w| w[0].height < w[1].height));
        assert_eq!(t[0].id, mainnet_latest_genesis());
    }

    fn mainnet_latest_genesis() -> [u8; 32] {
        let g = "6b7164fc";
        let t = mainnet();
        assert!(hex::encode(t[0].id).starts_with(g));
        t[0].id
    }
}
