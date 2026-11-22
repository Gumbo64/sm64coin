//! sealjson RUN.json [id]: the run of a statedump/real*.json (power-on start) sealed with seal_proof_at
//! (or codec `id`), printed as hex, with sizes on stderr.
use sm64pow::proof::{pad_from, Proof, Start};
use sm64pow::sm64_binds::GamePad;

fn hex(s: &str) -> Vec<u8> {
    (0..s.len() / 2).map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap()).collect()
}

fn main() -> anyhow::Result<()> {
    let a: Vec<String> = std::env::args().collect();
    sm64pow::init_default()?;
    let j: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&a[1])?)?;
    anyhow::ensure!(j["start"].as_u64() == Some(1), "power-on runs only");
    let seed = sm64pow::Seed::from_hex(j["seed_hash"].as_str().unwrap())?;
    let played: Vec<GamePad> = hex(j["played"].as_str().unwrap()).chunks_exact(4).map(pad_from).collect();
    let p = sm64pow::params(1);
    let blob = match a.get(2) {
        Some(id) => {
            let pr: Proof = Proof { frames: j["frames"].as_u64().unwrap() as u32, start: Start::PowerOn, inputs: hex(j["inputs"].as_str().unwrap()).chunks_exact(4).map(pad_from).collect() };
            sm64pow::statecodec::encode_proof_state(sm64pow::statecodec::state_codec(id.parse().unwrap()).unwrap(), None, &seed, &p, &pr)?
        }
        None => sm64pow::seal_proof_at(Start::PowerOn, None, &seed, &p, &played)?,
    };
    eprintln!("{} bytes (old proof {})", blob.len(), j["proof_bytes"]);
    println!("{}", blob.iter().map(|b| format!("{b:02x}")).collect::<String>());
    Ok(())
}
