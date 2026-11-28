//! The seg120 campaign's tables and choices (objective-rrt-v2's
//! tools/seg120/campaign.py), over the save file a state holds instead of the
//! campaign's flags text.
use super::Memory;
use obrrt_core::St;

pub const HUB: [u16; 3] = [6, 16, 26];

/// Level -> course number (campaign.py COURSE).
pub const COURSE: [(u16, u8); 24] = [
    (9, 1), (24, 2), (12, 3), (5, 4), (4, 5), (7, 6), (22, 7), (8, 8), (23, 9), (10, 10), (11, 11), (36, 12), (13, 13), (14, 14), (15, 15),
    (17, 16), (19, 17), (21, 18), (27, 19), (28, 20), (29, 21), (18, 22), (31, 23), (20, 24),
];

/// Stars a course holds (STARS_IN).
pub fn stars_in(course: u8) -> u32 {
    match course {
        1..=15 => 7,
        19 => 2,
        _ => 1,
    }
}

/// The Bowser courses' keys (KEY_OF): BitDW key 1, BitFS key 2.
pub const KEY_OF: [(u16, u8); 2] = [(17, 1), (19, 2)];
/// Cap-switch courses and the save flag their switch sets (CAP).
pub const CAP: [(u16, u32); 3] = [(29, 0x2), (28, 0x4), (18, 0x8)];
/// A cap course entered from another course (VIA): the metal cap from HMC.
pub const VIA: [(u16, u16); 1] = [(28, 7)];
/// Bowser courses -> their arenas (ARENA).
pub const ARENA: [(u16, u16); 3] = [(17, 30), (19, 33), (21, 34)];
/// Bowser in the Sky.
pub const BITS: u16 = 21;
/// SAVE_FLAG_MOAT_DRAINED.
pub const MOAT: u32 = 0x200;
/// Courses whose objectives steer by walking distance (GEO_LEVELS: the
/// races, SSL, WDW, HMC, BBH).
pub const GEO_LEVELS: [u16; 6] = [5, 13, 8, 11, 7, 4];
/// The castle's own stars, by save flag bit: (castle area, stars, keys) first
/// needed (CASTLE_STARS).
pub const CASTLE_STARS: [(u32, (u16, u8), i16, u8); 5] = [(24, (6, 3), 12, 1), (25, (6, 2), 25, 2), (26, (6, 2), 50, 2), (27, (6, 3), 15, 1), (28, (6, 3), 50, 1)];

/// What the doors ask before a main course can be entered, (stars, keys): the
/// star doors, the basement (key 1) and upstairs (key 2). `want_open` takes
/// only courses these let in; campaign.py had no such table and found out by
/// failing castle trees, which at 2 stars would cost a tree per closed course.
pub const DOORS: [(u16, i16, u8); 15] = [
    (9, 0, 0), (24, 1, 0), (12, 3, 0), (5, 3, 0), (4, 12, 0), (7, 0, 1), (22, 0, 1), (8, 0, 1), (23, 30, 1),
    (10, 0, 2), (11, 0, 2), (36, 0, 2), (13, 0, 2), (14, 50, 2), (15, 50, 2),
];

pub fn course_of(level: u16) -> Option<u8> {
    COURSE.iter().find(|c| c.0 == level).map(|c| c.1)
}

/// The stars byte of `level`'s course (0 for a level without one).
pub fn course_byte(st: &St, level: u16) -> u8 {
    course_of(level).map_or(0, |c| st.course_stars[c as usize - 1] & 0x7f)
}

pub fn arena(level: u16) -> u16 {
    ARENA.iter().find(|a| a.0 == level).map_or(level, |a| a.1)
}

pub fn via(level: u16) -> u16 {
    VIA.iter().find(|v| v.0 == level).map_or(level, |v| v.1)
}

pub fn all_courses() -> Vec<u16> {
    COURSE.iter().map(|c| c.0).collect()
}

/// All 120 stars and both keys: the last search beats Bowser 3.
pub fn final_(st: &St) -> bool {
    st.stars >= 120 && st.keys >= 2
}

/// Levels with nothing left to give: every star had, its key, its cap switch.
pub fn done_levels(st: &St) -> Vec<u16> {
    COURSE
        .iter()
        .filter(|&&(l, c)| {
            (st.course_stars[c as usize - 1] & 0x7f).count_ones() >= stars_in(c)
                && st.keys >= KEY_OF.iter().find(|k| k.0 == l).map_or(0, |k| k.1)
                && CAP.iter().find(|k| k.0 == l).is_none_or(|k| st.save_flags & k.1 != 0)
        })
        .map(|c| c.0)
        .collect()
}

/// Levels the castle doesn't go to: the skip list and the courses with
/// nothing left.
pub fn no_go(st: &St, m: &Memory) -> Vec<u16> {
    let mut v: Vec<u16> = m.skip.iter().copied().chain(done_levels(st)).collect();
    v.sort();
    v.dedup();
    v
}

/// Where the castle must go before anywhere else: Bowser in the Sky at the
/// end, else a cap course with its switch unpressed, else the open course
/// with the most stars missing.
pub fn want_target(st: &St, m: &Memory) -> Option<u16> {
    if final_(st) {
        return Some(BITS);
    }
    want_cap(st, m).or_else(|| want_open(st, m))
}

/// A cap course the campaign has been in (entered, or its star had) whose
/// switch is not pressed and not given up on.
pub fn want_cap(st: &St, m: &Memory) -> Option<u16> {
    CAP.iter()
        .find(|&&(l, bit)| st.save_flags & bit == 0 && (m.been.contains_key(&l) || course_byte(st, l) != 0) && !m.skip.contains(&l))
        .map(|c| c.0)
}

/// The next key course whose key the save is missing, unless a tree failed at it since the last full pass: a key
/// opens more stars than any one course, so it is tried before the most-missing-course pick.
pub fn want_key(st: &St, m: &Memory) -> Option<u16> {
    KEY_OF.iter().find(|&&(lvl, key)| st.keys < key && !m.skip.contains(&lvl)).map(|k| k.0)
}

/// The main course with the most stars missing, the least visited first,
/// among those the doors let in, not skipped and not failed.
pub fn want_open(st: &St, m: &Memory) -> Option<u16> {
    let nogo = no_go(st, m);
    let mut best: Option<((u32, i64), u16)> = None;
    for &(l, c) in COURSE.iter().filter(|c| (1..=15).contains(&c.1)) {
        let open = DOORS.iter().find(|d| d.0 == l).is_some_and(|d| st.stars >= d.1 && st.keys >= d.2);
        if !open || nogo.contains(&l) || m.open_fail.contains(&l) {
            continue;
        }
        let missing = stars_in(c) - (st.course_stars[c as usize - 1] & 0x7f).count_ones();
        let k = (missing, -(*m.been.get(&l).unwrap_or(&0) as i64));
        // max(): the first of the largest, in COURSE order.
        if best.is_none_or(|b| k > b.0) {
            best = Some((k, l));
        }
    }
    best.map(|b| b.1)
}

/// A castle star the save can have by now and hasn't, unless a tree failed
/// at it with these keys and fewer than 5 more stars: its castle area.
pub fn castle_star(st: &St, m: &Memory) -> Option<(u16, u8)> {
    if let Some((s, k)) = m.castle_try {
        if st.keys == k && st.stars < s + 5 {
            return None;
        }
    }
    CASTLE_STARS.iter().find(|&&(bit, _, n, k)| st.save_flags >> bit & 1 == 0 && st.stars >= n && st.keys >= k).map(|c| c.1)
}

/// The moat to drain before the vanish cap course: DDD's first star had, the
/// vanish switch not pressed, not drained, no failed try at these stars and keys.
pub fn moat_step(st: &St, m: &Memory) -> bool {
    m.moat_try != Some((st.stars, st.keys)) && st.save_flags & 0x8 == 0 && st.course_stars[8] & 1 != 0 && st.save_flags & MOAT == 0
}

pub fn name(level: u16) -> &'static str {
    match level {
        9 => "BoB", 24 => "WF", 12 => "JRB", 5 => "CCM", 4 => "BBH", 7 => "HMC", 22 => "LLL", 8 => "SSL", 23 => "DDD", 10 => "SL",
        11 => "WDW", 36 => "TTM", 13 => "THI", 14 => "TTC", 15 => "RR", 27 => "PSS", 6 => "Castle", 16 => "Grounds", 26 => "Courtyard",
        29 => "TotWC", 28 => "CotMC", 18 => "VCutM", 31 => "WMotR", 20 => "SA", 17 => "BitDW", 19 => "BitFS", 21 => "BitS", 30 => "Bowser1",
        33 => "Bowser2", 34 => "Bowser3", _ => "?",
    }
}
