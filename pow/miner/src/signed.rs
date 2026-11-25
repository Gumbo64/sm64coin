//! One run per key (v0.3): the miner key of a template may sign at most ONE run
//! for that template's seed. A second, different signed run under the same key
//! is equivocation evidence, and honest nodes avoid the miner's block. So before
//! signing, a miner asks this store; after deciding to sign, it records the run.
//! The record is a small JSON file (seed hash -> proof id and frames) that
//! survives restarts. Signing the very same run again is allowed (the signature
//! is deterministic, so it is the same block).

use anyhow::Result;
use std::collections::BTreeMap;
use std::path::PathBuf;

const KEEP: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Signed {
    /// `proof_id` of the signed run, hex.
    pub proof_id: String,
    pub frames: u32,
    /// Unix time of the record (to drop the oldest).
    #[serde(default)]
    pub at: u64,
}

#[derive(Debug)]
pub struct SignedStore {
    path: Option<PathBuf>,
    map: BTreeMap<String, Signed>,
}

impl SignedStore {
    /// Loads `path` (missing: empty). `None` keeps the record in memory only.
    pub fn open(path: Option<&str>) -> SignedStore {
        let path = path.filter(|p| !p.is_empty()).map(PathBuf::from);
        let map = path
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        SignedStore { path, map }
    }

    /// What was signed for `seed_hash`, if anything.
    pub fn get(&self, seed_hash: &str) -> Option<&Signed> {
        self.map.get(seed_hash)
    }

    /// May `proof` be signed for `seed_hash`? Yes if nothing was signed for it yet,
    /// or if the same run was. Otherwise returns what was signed.
    pub fn may_sign(&self, seed_hash: &str, proof: &[u8]) -> std::result::Result<(), Signed> {
        match self.map.get(seed_hash) {
            Some(s) if s.proof_id != hex::encode(sm64pow::proof_id(proof)) => Err(s.clone()),
            _ => Ok(()),
        }
    }

    /// Records that `proof` is about to be signed for `seed_hash`, and saves the file
    /// BEFORE the caller signs (a crash after this can only forbid a signature).
    pub fn record(&mut self, seed_hash: &str, proof: &[u8], frames: u32) -> Result<()> {
        let at = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        self.map.insert(seed_hash.to_string(), Signed { proof_id: hex::encode(sm64pow::proof_id(proof)), frames, at });
        while self.map.len() > KEEP {
            let oldest = self.map.iter().min_by_key(|(_, s)| s.at).map(|(k, _)| k.clone());
            match oldest {
                Some(k) => self.map.remove(&k),
                None => break,
            };
        }
        if let Some(p) = &self.path {
            if let Some(dir) = p.parent().filter(|d| !d.as_os_str().is_empty()) {
                std::fs::create_dir_all(dir)?;
            }
            let tmp = p.with_extension("tmp");
            std::fs::write(&tmp, serde_json::to_string(&self.map)?)?;
            std::fs::rename(&tmp, p)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_run_per_seed_and_it_survives_a_restart() {
        let dir = std::env::temp_dir().join(format!("signed-test-{}", std::process::id()));
        let f = dir.join("signed.json");
        let f = f.to_str().unwrap();
        let (a, b) = (vec![1u8, 2, 3], vec![9u8, 9, 9]);
        let mut s = SignedStore::open(Some(f));
        assert!(s.may_sign("seed", &a).is_ok());
        s.record("seed", &a, 100).unwrap();
        assert!(s.may_sign("seed", &a).is_ok(), "the same run again is the same block");
        assert!(s.may_sign("seed", &b).is_err(), "a second, different run is equivocation");
        assert!(s.may_sign("other", &b).is_ok());
        let s2 = SignedStore::open(Some(f));
        assert!(s2.may_sign("seed", &b).is_err(), "remembered after a restart");
        assert_eq!(s2.get("seed").unwrap().frames, 100);
        std::fs::remove_dir_all(dir).ok();
    }
}
