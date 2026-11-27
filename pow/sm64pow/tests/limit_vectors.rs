//! The time-limit vectors the browser build is checked against (`limit_vectors.json`, written by pow/perturb-wasm's test from
//! the same rules.rs file): these are sm64pow's answers, so the page, the desktop apps and the node agree at every timestamp.
use sm64pow::rules;

#[test]
fn the_time_limit_matches_the_shared_vectors() {
    let rows: Vec<(String, u64, u32)> = serde_json::from_str(include_str!("limit_vectors.json")).unwrap();
    assert!(rows.len() > 100);
    for (d, e, f) in rows {
        let d: u64 = d.parse().unwrap();
        assert_eq!(rules().max_frames(d, e), f, "difficulty {d}, {e} s after the parent");
    }
}
