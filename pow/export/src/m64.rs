//! The Mupen64 `.m64` movie format (version 3), power-on start, one controller.
//!
//! Header: 0x400 bytes (see `docs/EXPORT.md`), then 4 bytes per input poll:
//! the pad's 16 button bits big-endian (A is bit 7 of byte 0), stick x, stick y
//! (signed). That is exactly [`GamePad`]'s `button` as big-endian bytes, so no
//! bit shuffling is needed (the same layout as sm64-binds' `tas/*.m64`).

use anyhow::{Result, bail, ensure};
use sm64_binds::GamePad;

pub const HEADER_LEN: usize = 0x400;
pub const SIGNATURE: [u8; 4] = *b"M64\x1a";

/// What the header says about the ROM.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rom {
    /// Internal name (ROM header 0x20, 20 bytes; the m64 holds 32).
    pub name: Vec<u8>,
    /// CRC1: the 4 bytes at ROM header 0x10, read little-endian (stored verbatim in the m64).
    pub crc1: u32,
    /// Country code (ROM header 0x3E).
    pub country: u16,
}

impl Rom {
    /// From a big-endian (`.z64`) ROM image.
    pub fn from_z64(rom: &[u8]) -> Result<Rom> {
        ensure!(rom.len() >= 0x40, "ROM too short");
        ensure!(rom[0] == 0x80, "not a big-endian (.z64) ROM");
        let mut name = rom[0x20..0x34].to_vec();
        while name.last().is_some_and(|&b| b == 0 || b == b' ') {
            name.pop();
        }
        Ok(Rom {
            name,
            // The m64 header holds the ROM header's CRC1 bytes verbatim, so read them little-endian
            // (the same u32 an m64 writer on a little-endian host stores).
            crc1: u32::from_le_bytes(rom[0x10..0x14].try_into().unwrap()),
            country: rom[0x3E] as u16,
        })
    }

    /// The US Super Mario 64 values (for tests without a ROM).
    pub fn sm64_us() -> Rom {
        Rom { name: b"SUPER MARIO 64".to_vec(), crc1: 0xFF2B5A63, country: 0x45 }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Header {
    pub uid: u32,
    pub vi_count: u32,
    pub rerecords: u32,
    pub vis_per_second: u8,
    pub controllers: u8,
    pub samples: u32,
    /// 1 snapshot, 2 power-on, 4 EEPROM.
    pub start_type: u16,
    pub controller_flags: u32,
    pub rom: Rom,
    pub author: String,
    pub description: String,
}

fn put_str(h: &mut [u8], off: usize, len: usize, s: &[u8]) {
    let n = s.len().min(len - 1);
    h[off..off + n].copy_from_slice(&s[..n]);
}

impl Header {
    /// A power-on movie of `samples` polls. The game runs one poll per game
    /// frame and a frame is two VIs, so `vi_count` is `2 * samples`.
    pub fn power_on(samples: u32, uid: u32, rom: Rom, author: &str, description: &str) -> Header {
        Header {
            uid,
            vi_count: samples.saturating_mul(2),
            rerecords: 0,
            vis_per_second: 60,
            controllers: 1,
            samples,
            start_type: 2,
            controller_flags: 1,
            rom,
            author: author.into(),
            description: description.into(),
        }
    }

    pub fn to_bytes(&self) -> [u8; HEADER_LEN] {
        let mut h = [0u8; HEADER_LEN];
        h[0..4].copy_from_slice(&SIGNATURE);
        h[4..8].copy_from_slice(&3u32.to_le_bytes());
        h[8..12].copy_from_slice(&self.uid.to_le_bytes());
        h[12..16].copy_from_slice(&self.vi_count.to_le_bytes());
        h[16..20].copy_from_slice(&self.rerecords.to_le_bytes());
        h[20] = self.vis_per_second;
        h[21] = self.controllers;
        h[24..28].copy_from_slice(&self.samples.to_le_bytes());
        h[28..30].copy_from_slice(&self.start_type.to_le_bytes());
        h[32..36].copy_from_slice(&self.controller_flags.to_le_bytes());
        put_str(&mut h, 0xC4, 32, &self.rom.name);
        h[0xE4..0xE8].copy_from_slice(&self.rom.crc1.to_le_bytes());
        h[0xE8..0xEA].copy_from_slice(&self.rom.country.to_le_bytes());
        put_str(&mut h, 0x122, 64, b"no video plugin (headless sm64-binds)");
        put_str(&mut h, 0x162, 64, b"no audio plugin");
        put_str(&mut h, 0x1A2, 64, b"m64 input");
        put_str(&mut h, 0x1E2, 64, b"HLE RSP");
        put_str(&mut h, 0x222, 222, self.author.as_bytes());
        put_str(&mut h, 0x300, 256, self.description.as_bytes());
        h
    }

    pub fn parse(b: &[u8]) -> Result<Header> {
        ensure!(b.len() >= HEADER_LEN, "m64 shorter than its header");
        ensure!(b[0..4] == SIGNATURE, "not an m64 (bad signature)");
        let u32_at = |o: usize| u32::from_le_bytes(b[o..o + 4].try_into().unwrap());
        let u16_at = |o: usize| u16::from_le_bytes(b[o..o + 2].try_into().unwrap());
        ensure!(u32_at(4) == 3, "m64 version {} (only 3 is supported)", u32_at(4));
        let cstr = |o: usize, n: usize| {
            let s = &b[o..o + n];
            let e = s.iter().position(|&c| c == 0).unwrap_or(n);
            s[..e].to_vec()
        };
        Ok(Header {
            uid: u32_at(8),
            vi_count: u32_at(12),
            rerecords: u32_at(16),
            vis_per_second: b[20],
            controllers: b[21],
            samples: u32_at(24),
            start_type: u16_at(28),
            controller_flags: u32_at(32),
            rom: Rom { name: cstr(0xC4, 32), crc1: u32_at(0xE4), country: u16_at(0xE8) },
            author: String::from_utf8_lossy(&cstr(0x222, 222)).into(),
            description: String::from_utf8_lossy(&cstr(0x300, 256)).into(),
        })
    }
}

/// One poll as the m64 stores it: buttons big-endian, stick x, stick y.
pub fn pad_bytes(p: GamePad) -> [u8; 4] {
    let b = p.button.to_be_bytes();
    [b[0], b[1], p.stick_x as u8, p.stick_y as u8]
}

pub fn pad_from_bytes(b: [u8; 4]) -> GamePad {
    GamePad::new(u16::from_be_bytes([b[0], b[1]]), b[2] as i8, b[3] as i8)
}

/// The headerless frames (the `.pads` files).
pub fn frames_bytes(pads: &[GamePad]) -> Vec<u8> {
    let mut out = Vec::with_capacity(pads.len() * 4);
    for p in pads {
        out.extend_from_slice(&pad_bytes(*p));
    }
    out
}

pub fn frames_from_bytes(b: &[u8]) -> Result<Vec<GamePad>> {
    if b.len() % 4 != 0 {
        bail!("input data is not a multiple of 4 bytes");
    }
    Ok(b.chunks_exact(4).map(|c| pad_from_bytes(c.try_into().unwrap())).collect())
}

/// The N64 controller's two reserved button bits (0x0080, 0x0040). The game never reads them
/// (sm64pow tests/reserved_bits.rs), but Mupen64Plus reads a sample with both set as a console
/// reset, so a movie containing one doesn't play back. The poke pads only use A, B and Z, and the
/// miners no longer seal such pads; old chains may still have them.
pub const RESERVED_BITS: u16 = 0x00C0;

/// `pads` with the reserved bits cleared on every pad that has both set.
pub fn clear_reserved_bits(pads: &[GamePad]) -> Vec<GamePad> {
    pads.iter().map(|p| if p.button & RESERVED_BITS == RESERVED_BITS { GamePad::new(p.button & !RESERVED_BITS, p.stick_x, p.stick_y) } else { *p }).collect()
}

/// A whole movie: header, then the frames.
pub fn write(header: &Header, pads: &[GamePad]) -> Vec<u8> {
    let mut h = header.clone();
    h.samples = pads.len() as u32;
    h.vi_count = h.samples.saturating_mul(2);
    let mut out = h.to_bytes().to_vec();
    out.extend(frames_bytes(pads));
    out
}

/// Reads a movie back: the header and exactly `samples` frames (a movie may
/// have trailing input past `samples`, which players ignore).
pub fn read(b: &[u8]) -> Result<(Header, Vec<GamePad>)> {
    let h = Header::parse(b)?;
    let body = &b[HEADER_LEN..];
    ensure!(body.len() >= h.samples as usize * 4, "m64 holds {} bytes of input, its header says {} samples", body.len(), h.samples);
    Ok((h.clone(), frames_from_bytes(&body[..h.samples as usize * 4])?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let pads = vec![GamePad::new(0x8000, 80, -80), GamePad::new(0x0120, -128, 127), GamePad::default()];
        let h = Header::power_on(0, 7, Rom::sm64_us(), "me", "desc");
        let b = write(&h, &pads);
        assert_eq!(b.len(), HEADER_LEN + 12);
        let (h2, p2) = read(&b).unwrap();
        assert_eq!(p2, pads);
        assert_eq!(h2.samples, 3);
        assert_eq!(h2.vi_count, 6);
        assert_eq!((h2.rom, h2.author.as_str(), h2.start_type), (Rom::sm64_us(), "me", 2));
    }

    #[test]
    fn a_is_bit_7_of_byte_0() {
        assert_eq!(pad_bytes(GamePad::new(sm64_binds::buttons::A_BUTTON, 1, 2)), [0x80, 0, 1, 2]);
        assert_eq!(pad_from_bytes([0x20, 0x04, 0xff, 0x01]), GamePad::new(0x2004, -1, 1));
    }

    /// A real TASVideos movie's header (sm64-binds tas/) parses, when present.
    #[test]
    fn parses_a_real_movie_header() {
        let p = "../../../sm64-binds/tas/tasvideos-2016M-0star.m64";
        let Ok(b) = std::fs::read(p) else { return };
        let (h, pads) = read(&b).unwrap();
        assert_eq!((h.samples, h.start_type, h.rom.clone()), (8772, 2, Rom::sm64_us()));
        assert_eq!(pads.len(), 8772);
        let mut again = h.clone();
        again.author = "x".into();
        assert_eq!(Header::parse(&again.to_bytes()).unwrap().rom, h.rom);
    }
}

#[cfg(test)]
mod reserved_tests {
    use super::*;

    #[test]
    fn only_pads_with_both_reserved_bits_change() {
        let pads = [GamePad::new(0x8000 | RESERVED_BITS, 1, 2), GamePad::new(0x0080, 3, 4), GamePad::new(0x0040, 5, 6), GamePad::new(0x8000, 7, 8)];
        let c = clear_reserved_bits(&pads);
        assert_eq!(c[0], GamePad::new(0x8000, 1, 2));
        assert_eq!(&c[1..], &pads[1..]);
    }
}
