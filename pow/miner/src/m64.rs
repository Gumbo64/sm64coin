use anyhow::Result;
use sm64_binds::GamePad;

/// Inputs from a Mupen64 movie (0x400-byte header) or a raw file: 4 bytes a
/// frame, u16 big-endian buttons, i8 stick x, i8 stick y.
pub fn load_m64(path: &str) -> Result<Vec<GamePad>> {
    let bytes = std::fs::read(path)?;
    let body = if bytes.len() >= 0x400 && &bytes[..4] == b"M64\x1a" { &bytes[0x400..] } else { &bytes[..] };
    Ok(body
        .chunks_exact(4)
        .map(|c| GamePad::new(u16::from_be_bytes([c[0], c[1]]), c[2] as i8, c[3] as i8))
        .collect())
}
