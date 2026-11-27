//! The wallet PIN (desktop quick-unlock): a 4-8 digit PIN wraps the wallet's password so the wallet opens without typing it.
//!
//! This is a CONVENIENCE lock, not strong protection: PBKDF2-HMAC-SHA256 (600 000 rounds) turns the PIN into an AES-256-GCM key
//! that seals the password, but there are only 10^4 to 10^8 PINs, so whoever copies this file can try them all offline. What the
//! attempt counter does limit is guessing through the app: after `tries` wrong PINs (default 5) the file is wiped and only the
//! real password (or the seed words) opens the wallet. The file is `<data>/wallet-pins/<wallet>.pin`, mode 0600, and holds
//! nothing but the sealed password and the counter. The wallet name is bound in as AEAD data (a file cannot be moved to another
//! wallet). Passwords and PINs are never logged and are wiped (`Zeroizing`) after use. wallet-rpc still enforces its own checks:
//! the unwrapped password is only ever handed to it, never to the page.
//!
//! Pure (no Tauri types), so the host's tests run it everywhere.
use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::path::Path;
use zeroize::Zeroizing;

pub const PIN_ITER: u32 = 600_000;
pub const PIN_TRIES: u32 = 5;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PinRecord {
    pub v: u32,
    pub kdf: String,
    pub iter: u32,
    pub salt: String,
    pub nonce: String,
    pub ct: String,
    /// Wrong PINs allowed in a row; the one that reaches it wipes the record.
    pub tries: u32,
    pub fails: u32,
    /// The password was made by the quick-unlock setup (the user does not know it): disabling must set a new one.
    pub generated: bool,
}

pub fn valid_pin(p: &str) -> bool {
    (4..=8).contains(&p.len()) && p.bytes().all(|c| c.is_ascii_digit())
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}
fn unhex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 || !s.is_ascii() {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok()).collect()
}
fn rand<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    getrandom::getrandom(&mut b).expect("the system random generator is unavailable");
    b
}
fn derive(pin: &str, salt: &[u8], iter: u32) -> Zeroizing<[u8; 32]> {
    let mut k = Zeroizing::new([0u8; 32]);
    pbkdf2::pbkdf2_hmac::<Sha256>(pin.as_bytes(), salt, iter, &mut *k);
    k
}

/// Seals `secret` under `pin`; `aad` is the wallet's name.
pub fn wrap(secret: &str, pin: &str, aad: &str, iter: u32, tries: u32, generated: bool) -> Result<PinRecord, String> {
    if !valid_pin(pin) {
        return Err("a PIN is 4 to 8 digits".into());
    }
    let (salt, nonce) = (rand::<16>(), rand::<12>());
    let key = derive(pin, &salt, iter);
    let ct = Aes256Gcm::new_from_slice(&*key)
        .map_err(|e| e.to_string())?
        .encrypt(Nonce::from_slice(&nonce), Payload { msg: secret.as_bytes(), aad: aad.as_bytes() })
        .map_err(|_| "could not seal the password".to_string())?;
    Ok(PinRecord { v: 1, kdf: "pbkdf2-sha256".into(), iter, salt: hex(&salt), nonce: hex(&nonce), ct: hex(&ct), tries, fails: 0, generated })
}

/// The sealed secret, or None for a wrong PIN (or a damaged record). Does not touch the counter: see `attempt`.
pub fn unwrap(rec: &PinRecord, pin: &str, aad: &str) -> Option<Zeroizing<String>> {
    if rec.v != 1 || rec.kdf != "pbkdf2-sha256" || !valid_pin(pin) || rec.iter == 0 || rec.iter > 100_000_000 {
        return None;
    }
    let (salt, nonce, ct) = (unhex(&rec.salt)?, unhex(&rec.nonce)?, unhex(&rec.ct)?);
    if nonce.len() != 12 {
        return None;
    }
    let key = derive(pin, &salt, rec.iter);
    let pt = Aes256Gcm::new_from_slice(&*key).ok()?.decrypt(Nonce::from_slice(&nonce), Payload { msg: &ct, aad: aad.as_bytes() }).ok()?;
    String::from_utf8(pt).ok().map(Zeroizing::new)
}

pub enum Attempt {
    /// The right PIN: the sealed secret (the counter is reset).
    Ok(Zeroizing<String>),
    /// A wrong PIN: this many tries are left.
    Wrong { left: u32 },
    /// A wrong PIN that used the last try (or no/damaged record): the file is gone, the password is needed.
    Wiped,
}

/// One PIN try against the record in `path`. The failure is counted (and written) BEFORE the PIN is checked, so a process
/// killed in the middle does not give a free guess; success resets the count; the last wrong try deletes the file.
pub fn attempt(path: &Path, pin: &str, aad: &str) -> Result<Attempt, String> {
    let Some(mut rec) = load(path) else { return Ok(Attempt::Wiped) };
    let tries = rec.tries.clamp(1, 100);
    rec.fails = rec.fails.saturating_add(1);
    let counted = rec.clone();
    save(path, &counted)?;
    match unwrap(&counted, pin, aad) {
        Some(secret) => {
            let mut ok = counted;
            ok.fails = 0;
            save(path, &ok)?;
            Ok(Attempt::Ok(secret))
        }
        None if counted.fails >= tries => {
            remove(path);
            Ok(Attempt::Wiped)
        }
        None => Ok(Attempt::Wrong { left: tries - counted.fails }),
    }
}

pub fn load(path: &Path) -> Option<PinRecord> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

/// Writes the record atomically (temp file then rename), readable by this user only.
pub fn save(path: &Path, rec: &PinRecord) -> Result<(), String> {
    use std::io::Write;
    let tmp = path.with_extension("pin.tmp");
    let _ = std::fs::remove_file(&tmp);
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    let json = serde_json::to_string(rec).map_err(|e| e.to_string())?;
    o.open(&tmp).and_then(|mut f| f.write_all(json.as_bytes()).and_then(|_| f.sync_all())).map_err(|e| format!("cannot write the PIN file: {e}"))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("cannot write the PIN file: {e}"))
}

pub fn remove(path: &Path) {
    let _ = std::fs::remove_file(path);
}

#[cfg(test)]
mod tests {
    use super::*;

    const FAST: u32 = 1000; // the real 600 000 rounds are slow in a debug build

    fn dir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("sm64coin-pin-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn pin_rule() {
        assert!(valid_pin("1234") && valid_pin("12345678"));
        assert!(!valid_pin("123") && !valid_pin("123456789") && !valid_pin("12a4") && !valid_pin("") && !valid_pin("١٢٣٤"));
        assert!(wrap("pw", "12", "w", FAST, 5, false).is_err());
    }

    #[test]
    fn wrap_and_unwrap() {
        let r = wrap("correct horse", "4821", "main", FAST, 5, true).unwrap();
        assert!(!r.ct.contains("correct") && r.generated && r.fails == 0);
        assert_eq!(unwrap(&r, "4821", "main").unwrap().as_str(), "correct horse");
        assert!(unwrap(&r, "4822", "main").is_none(), "wrong PIN");
        assert!(unwrap(&r, "4821", "other").is_none(), "another wallet's name");
        // two wraps of the same secret differ (fresh salt and nonce)
        let r2 = wrap("correct horse", "4821", "main", FAST, 5, true).unwrap();
        assert!(r.salt != r2.salt && r.ct != r2.ct);
        let mut bad = r.clone();
        bad.ct = "00".repeat(r.ct.len() / 2);
        assert!(unwrap(&bad, "4821", "main").is_none());
    }

    #[test]
    fn five_wrong_pins_wipe_the_file() {
        let d = dir("limit");
        let p = d.join("main.pin");
        save(&p, &wrap("secret-pw", "2468", "main", FAST, 5, false).unwrap()).unwrap();
        for left in (1..=4).rev() {
            match attempt(&p, "0000", "main").unwrap() {
                Attempt::Wrong { left: l } => assert_eq!(l, left),
                _ => panic!("expected a wrong try"),
            }
            assert!(p.exists());
        }
        assert!(matches!(attempt(&p, "0000", "main").unwrap(), Attempt::Wiped));
        assert!(!p.exists(), "wiped");
        // and afterwards even the right PIN gets nothing
        assert!(matches!(attempt(&p, "2468", "main").unwrap(), Attempt::Wiped));
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn a_right_pin_resets_the_count() {
        let d = dir("reset");
        let p = d.join("main.pin");
        save(&p, &wrap("secret-pw", "2468", "main", FAST, 5, false).unwrap()).unwrap();
        for _ in 0..4 {
            assert!(matches!(attempt(&p, "1111", "main").unwrap(), Attempt::Wrong { .. }));
        }
        assert_eq!(load(&p).unwrap().fails, 4);
        match attempt(&p, "2468", "main").unwrap() {
            Attempt::Ok(s) => assert_eq!(s.as_str(), "secret-pw"),
            _ => panic!("right PIN"),
        }
        assert_eq!(load(&p).unwrap().fails, 0);
        // four more wrong ones are fine again
        for _ in 0..4 {
            assert!(matches!(attempt(&p, "1111", "main").unwrap(), Attempt::Wrong { .. }));
        }
        assert!(p.exists());
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn the_count_is_written_before_the_check() {
        // a try that never finishes (process killed) still counted: simulate by checking the file after a wrong try
        let d = dir("order");
        let p = d.join("main.pin");
        save(&p, &wrap("pw", "2468", "main", FAST, 3, false).unwrap()).unwrap();
        attempt(&p, "1", "main").unwrap(); // not even a valid PIN: counts
        assert_eq!(load(&p).unwrap().fails, 1);
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn the_file_is_private_and_holds_no_secret() {
        use std::os::unix::fs::PermissionsExt;
        let d = dir("mode");
        let p = d.join("main.pin");
        save(&p, &wrap("hunter2-hunter2", "2468", "main", FAST, 5, false).unwrap()).unwrap();
        assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(!text.contains("hunter2") && !text.contains("2468"));
        assert!(!d.join("main.pin.tmp").exists());
        std::fs::remove_dir_all(&d).unwrap();
    }
}
