//! On wasm32-unknown-unknown (the web miner) `std::time::Instant::now()` and `SystemTime::now()` panic
//! ("time not implemented on this platform"). v0.5.0–v0.5.6 called one in the hand-off window, so every
//! web worker whose search met its goal died and restarted. Code the browser runs must use
//! `crate::sys::Instant` (web-time in the browser) and `crate::unix_secs()`.
//!
//! Native-only code is exempt: `campaign.rs` (`#[cfg(not(target_arch = "wasm32"))]`), the command-line
//! programs in `bin/`, and the native branch of `unix_secs` / `sys` in `lib.rs`.

use std::path::Path;

fn rust_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            rust_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

#[test]
fn browser_code_does_not_use_std_clock() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&src, &mut files);
    let mut bad = Vec::new();
    for f in files {
        let rel = f.strip_prefix(&src).unwrap().to_string_lossy().replace('\\', "/");
        if rel == "campaign.rs" || rel == "lib.rs" || rel.starts_with("bin/") {
            continue;
        }
        let text = std::fs::read_to_string(&f).unwrap();
        for (i, line) in text.lines().enumerate() {
            let code = line.split("//").next().unwrap_or("");
            // Every `std::time::…` path or import up to its `;` (catches `use std::time::{Duration, Instant}` too).
            let std_clock = code.match_indices("std::time::").any(|(at, _)| {
                let rest = &code[at..];
                let seg = &rest[..rest.find(';').unwrap_or(rest.len())];
                seg.contains("Instant") || seg.contains("SystemTime")
            });
            if std_clock || code.contains("SystemTime::now") {
                bad.push(format!("src/{rel}:{}: {}", i + 1, line.trim()));
            }
        }
    }
    assert!(bad.is_empty(), "std's clock panics in the web miner; use crate::sys::Instant / crate::unix_secs():\n{}", bad.join("\n"));
}
