//! Where the block should go: the seg120 campaign's choices (tools/seg120/campaign.py)
//! for one block, which is one progress point. In the castle: the main course
//! with the most stars missing (ties: the nearer one), not one the save file's
//! star count keeps shut or the block already gave up on. In a course: its tree
//! runs until a state has more stars/keys than the start.
//!
//! Not ported (the campaign keeps state across steps, a block has none):
//! the skip list across blocks, checkpoints, coin legs, cap-switch courses,
//! Bowser arenas and the Grand Star's final search. Courses the star counts
//! keep shut are only guessed (`NEEDS`); a wrong guess costs one failed castle
//! tree (the target is then banned for the block).
use obrrt_core::St;

pub const HUB: [u16; 3] = [6, 16, 26];

/// Course levels in the order the castle reaches them (nearest first), with
/// the course number and the stars a visit needs (the star doors; approximate
/// above the first floor).
pub const COURSES: [(u16, i16, i16); 15] = [
    (9, 1, 0),   // BoB
    (24, 2, 1),  // WF
    (12, 3, 3),  // JRB
    (5, 4, 3),   // CCM
    (4, 5, 8),   // BBH
    (7, 6, 8),   // HMC
    (22, 7, 8),  // LLL
    (8, 8, 8),   // SSL
    (23, 9, 8),  // DDD
    (10, 10, 30), // SL
    (11, 11, 30), // WDW
    (36, 12, 30), // TTM
    (13, 13, 50), // THI
    (14, 14, 50), // TTC
    (15, 15, 50), // RR
];

/// Stars (0-6 of 7) the save file has in `course`.
pub fn got(st: &St, course: i16) -> usize {
    (0..7u8).filter(|&i| st.has_star(course, i)).count()
}

/// The levels to try from the castle, best first, without `banned`.
pub fn targets(st: &St, banned: &[u16]) -> Vec<u16> {
    let mut v: Vec<(u16, i16, i16)> = COURSES.iter().copied().filter(|&(l, _, need)| !banned.contains(&l) && st.stars >= need).collect();
    // The most stars missing; the nearer course first on ties.
    v.sort_by_key(|&(_, c, _)| std::cmp::Reverse(7 - got(st, c) as i32));
    v.into_iter().filter(|&(_, c, _)| got(st, c) < 7).map(|x| x.0).collect()
}

/// Every course level except `keep` (the castle tree's EXIT_SKIP).
pub fn skip_all_but(keep: Option<u16>) -> Vec<u16> {
    let mut v: Vec<u16> = COURSES.iter().map(|c| c.0).filter(|l| Some(*l) != keep).collect();
    // The cap courses and the Bowser doors are never targets.
    v.extend([29, 28, 18, 17, 19, 21, 31, 20, 27]);
    v
}

pub fn name(level: u16) -> &'static str {
    match level {
        9 => "BoB", 24 => "WF", 12 => "JRB", 5 => "CCM", 4 => "BBH", 7 => "HMC", 22 => "LLL", 8 => "SSL", 23 => "DDD", 10 => "SL",
        11 => "WDW", 36 => "TTM", 13 => "THI", 14 => "TTC", 15 => "RR", 6 => "Castle", 16 => "Grounds", 26 => "Courtyard", _ => "?",
    }
}
