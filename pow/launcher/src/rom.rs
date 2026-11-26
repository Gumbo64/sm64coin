//! Finding, verifying and installing the user's Super Mario 64 ROM (US, SHA-1 checked).
use crate::util::{home, plain};
use sha1::{Digest, Sha1};
use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

pub const ROM_SHA1: &str = "9bef1128717f958171a4afac3ed78ee2bb4e86ce";
pub const ROM_NAME: &str = "baserom.us.z64";

pub fn sha1_file(p: &Path) -> std::io::Result<String> {
    let mut f = File::open(p)?;
    let mut h = Sha1::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}


/// Look for the ROM; Ok(path) when one with the right SHA-1 is found.
pub fn find(arg: Option<&Path>, exe_dir: &Path, data_dir: &Path) -> Result<PathBuf, String> {
    let mut files: Vec<PathBuf> = Vec::new();
    let mut dirs: Vec<PathBuf> = Vec::new();
    let add = |p: PathBuf, files: &mut Vec<PathBuf>, dirs: &mut Vec<PathBuf>| {
        if p.is_dir() { dirs.push(p) } else { files.push(p) }
    };
    if let Some(a) = arg {
        add(a.to_path_buf(), &mut files, &mut dirs);
        if !a.exists() {
            return Err(format!("the ROM path {} does not exist", a.display()));
        }
    }
    if let Some(e) = std::env::var_os("SM64COIN_ROM") {
        add(PathBuf::from(e), &mut files, &mut dirs);
    }
    let cwd = std::env::current_dir().unwrap_or_default();
    for d in [exe_dir.to_path_buf(), cwd, home().join(".sm64coin"), data_dir.to_path_buf()] {
        dirs.push(d);
    }
    for d in &dirs {
        files.push(d.join(ROM_NAME));
    }
    // Any other .z64 in those folders (a renamed copy, "Super Mario 64 (U) [!].z64").
    for d in &dirs {
        if let Ok(rd) = fs::read_dir(d) {
            let mut v: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("z64"))).collect();
            v.sort();
            files.extend(v);
        }
    }
    let mut seen = BTreeSet::new();
    let mut wrong: Vec<(PathBuf, String)> = Vec::new();
    for f in files {
        let key = plain(fs::canonicalize(&f).unwrap_or_else(|_| f.clone()));
        if !f.is_file() || !seen.insert(key.clone()) {
            continue;
        }
        match sha1_file(&f) {
            Ok(h) if h == ROM_SHA1 => return Ok(key),
            Ok(h) => wrong.push((f, h)),
            Err(_) => {}
        }
    }
    let mut msg = String::new();
    msg.push_str("Could not find your Super Mario 64 ROM.\n\n");
    msg.push_str(&format!("SM64Coin needs the US version of Super Mario 64 as a .z64 file (SHA-1 {ROM_SHA1}).\n"));
    msg.push_str("It is not included: you have to supply your own copy.\n\n");
    for (p, h) in &wrong {
        msg.push_str(&format!("  Found {} but it is not the US ROM (its SHA-1 is {h}).\n", p.display()));
    }
    msg.push_str(&format!(
        "Do one of these and run sm64coin again:\n  - copy it next to this program, named {ROM_NAME}  ({})\n  - or copy it to {}\n  - or give its path:  sm64coin /path/to/rom.z64\n",
        exe_dir.display(),
        home().join(".sm64coin").join(ROM_NAME).display()
    ));
    Err(msg)
}

/// Is `p` the right ROM? Ok(true) when its SHA-1 matches.
pub fn verify(p: &Path) -> Result<bool, String> {
    sha1_file(p).map(|h| h == ROM_SHA1).map_err(|e| format!("cannot read {}: {e}", p.display()))
}

/// Verifies `src` and copies it to `<data_dir>/baserom.us.z64`; returns the installed path.
pub fn install(src: &Path, data_dir: &Path) -> Result<PathBuf, String> {
    if !verify(src)? {
        return Err(format!("{} is not the US Super Mario 64 ROM (SHA-1 {ROM_SHA1})", src.display()));
    }
    fs::create_dir_all(data_dir).map_err(|e| format!("cannot create {}: {e}", data_dir.display()))?;
    let dst = data_dir.join(ROM_NAME);
    if dst.exists() && fs::canonicalize(&dst).ok() == fs::canonicalize(src).ok() {
        return Ok(dst);
    }
    fs::copy(src, &dst).map_err(|e| format!("cannot copy the ROM to {}: {e}", dst.display()))?;
    Ok(dst)
}
