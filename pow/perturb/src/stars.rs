//! The star board: which of the game's 120 stars a state has and which are left, for the page (the Mine page's board) and
//! anything else that wants to show it. Read from the same save-file bytes the progress points come from
//! ([`crate::sm64::progress`] -> [`Progress::star_flags`]), so there is one reader for nodes, the miner and the page.
//! Names only; no game data.
use serde::Serialize;

use crate::sm64::Progress;

/// One row of the board: `(name, short, [star names])`. Rows 1..=15 are the courses (the save file's course 1..=15, seven stars
/// each, the seventh the 100-coin star); then the castle's own stars (save "course" 0) and the other courses' stars together.
const COURSES: [(&str, &str, [&str; 7]); 15] = [
    ("Bob-omb Battlefield", "BoB", ["Big Bob-omb on the Summit", "Footrace with Koopa the Quick", "Shoot to the Island in the Sky", "Find the 8 Red Coins", "Mario Wings to the Sky", "Behind Chain Chomp's Gate", "100 Coins"]),
    ("Whomp's Fortress", "WF", ["Chip Off Whomp's Block", "To the Top of the Fortress", "Shoot into the Wild Blue", "Red Coins on the Floating Isle", "Fall onto the Caged Island", "Blast Away the Wall", "100 Coins"]),
    ("Jolly Roger Bay", "JRB", ["Plunder in the Sunken Ship", "Can the Eel Come Out to Play?", "Treasure of the Ocean Cave", "Red Coins on the Ship Afloat", "Blast to the Stone Pillar", "Through the Jet Stream", "100 Coins"]),
    ("Cool, Cool Mountain", "CCM", ["Slip Slidin' Away", "Li'l Penguin Lost", "Big Penguin Race", "Frosty Slide for 8 Red Coins", "Snowman's Lost His Head", "Wall Kicks Will Work", "100 Coins"]),
    ("Big Boo's Haunt", "BBH", ["Go on a Ghost Hunt", "Ride Big Boo's Merry-Go-Round", "Secret of the Haunted Books", "Seek the 8 Red Coins", "Big Boo's Balcony", "Eye to Eye in the Secret Room", "100 Coins"]),
    ("Hazy Maze Cave", "HMC", ["Swimming Beast in the Cavern", "Elevate for 8 Red Coins", "Metal-Head Mario Can Move!", "Navigating the Toxic Maze", "A-Maze-Ing Emergency Exit", "Watch for Rolling Rocks", "100 Coins"]),
    ("Lethal Lava Land", "LLL", ["Boil the Big Bully", "Bully the Bullies", "8-Coin Puzzle with 15 Pieces", "Red-Hot Log Rolling", "Hot-Foot-It into the Volcano", "Elevator Tour in the Volcano", "100 Coins"]),
    ("Shifting Sand Land", "SSL", ["In the Talons of the Big Bird", "Shining Atop the Pyramid", "Inside the Ancient Pyramid", "Stand Tall on the Four Pillars", "Free Flying for 8 Red Coins", "Pyramid Puzzle", "100 Coins"]),
    ("Dire, Dire Docks", "DDD", ["Board Bowser's Sub", "Chests in the Current", "Pole-Jumping for Red Coins", "Through the Jet Stream", "The Manta Ray's Reward", "Collect the Caps...", "100 Coins"]),
    ("Snowman's Land", "SL", ["Snowman's Big Head", "Chill with the Bully", "In the Deep Freeze", "Whirl from the Freezing Pond", "Shell Shreddin' for Red Coins", "Into the Igloo", "100 Coins"]),
    ("Wet-Dry World", "WDW", ["Shocking Arrow Lifts!", "Top o' the Town", "Secrets in the Shallows & Sky", "Express Elevator--Hurry Up!", "Go to Town for Red Coins", "Quick Race Through Downtown!", "100 Coins"]),
    ("Tall, Tall Mountain", "TTM", ["Scale the Mountain", "Mystery of the Monkey Cage", "Scary 'Shrooms, Red Coins", "Mysterious Mountainside", "Breathtaking View from Bridge", "Blast to the Lonely Mushroom", "100 Coins"]),
    ("Tiny-Huge Island", "THI", ["Pluck the Piranha Flower", "The Tip Top of the Huge Island", "Rematch with Koopa the Quick", "Five Itty Bitty Secrets", "Wiggler's Red Coins", "Make Wiggler Squirm", "100 Coins"]),
    ("Tick Tock Clock", "TTC", ["Roll into the Cage", "The Pit and the Pendulums", "Get a Hand", "Stomp on the Thwomp", "Timed Jumps on Moving Bars", "Stop Time for Red Coins", "100 Coins"]),
    ("Rainbow Ride", "RR", ["Cruiser Crossing the Rainbow", "The Big House in the Sky", "Coins Amassed in a Maze", "Swingin' in the Breeze", "Tricky Triangles!", "Somewhere Over the Rainbow", "100 Coins"]),
];

/// The castle's own stars: save "course" 0, bit k = star k+1 (the three Toads, then the two MIPS).
const CASTLE: [&str; 5] = ["Toad (basement)", "Toad (upstairs)", "Toad (tower)", "MIPS (first)", "MIPS (second)"];

/// The other courses' stars: `(save course 16..=24, name, star count)`.
const SECRET: [(usize, &str, u8); 9] = [
    (16, "Bowser in the Dark World", 1),
    (17, "Bowser in the Fire Sea", 1),
    (18, "Bowser in the Sky", 1),
    (19, "The Princess's Secret Slide", 2),
    (20, "Cavern of the Metal Cap", 1),
    (21, "Tower of the Wing Cap", 1),
    (22, "Vanish Cap Under the Moat", 1),
    (23, "Wing Mario Over the Rainbow", 1),
    (24, "The Secret Aquarium", 1),
];

/// The princess's slide's second star.
const SLIDE_FAST: &str = "The Princess's Secret Slide: under 21 seconds";

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Star {
    pub name: String,
    pub got: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Group {
    pub name: String,
    pub short: String,
    pub stars: Vec<Star>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Switch {
    pub name: String,
    pub got: bool,
}

/// `{got, total, groups: [{name, short, stars: [{name, got}]}], switches: [{name, got}]}`: the 15 courses, then the castle's
/// stars, then the other courses' stars; the switches are the three cap switches and the two Bowser keys.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Board {
    pub got: u32,
    pub total: u32,
    pub groups: Vec<Group>,
    pub switches: Vec<Switch>,
}

/// The board of a state's [`Progress`].
pub fn board(p: &Progress) -> Board {
    let bit = |course: usize, k: usize| p.star_flags[course] >> k & 1 != 0;
    let mut groups: Vec<Group> = COURSES
        .iter()
        .enumerate()
        .map(|(i, (name, short, stars))| Group { name: (*name).into(), short: (*short).into(), stars: stars.iter().enumerate().map(|(k, s)| Star { name: (*s).into(), got: bit(i + 1, k) }).collect() })
        .collect();
    groups.push(Group { name: "Peach's Castle".into(), short: "Castle".into(), stars: CASTLE.iter().enumerate().map(|(k, s)| Star { name: (*s).into(), got: bit(0, k) }).collect() });
    let mut secret = Vec::new();
    for (course, name, n) in SECRET {
        for k in 0..n as usize {
            let name = if course == 19 && k == 1 { SLIDE_FAST } else { name };
            secret.push(Star { name: name.into(), got: bit(course, k) });
        }
    }
    groups.push(Group { name: "Secret courses and Bowser stages".into(), short: "Secret".into(), stars: secret });
    let got = groups.iter().map(|g| g.stars.iter().filter(|s| s.got).count() as u32).sum();
    let total = groups.iter().map(|g| g.stars.len() as u32).sum();
    let mut switches: Vec<Switch> = ["wing cap", "metal cap", "vanish cap"].iter().enumerate().map(|(i, n)| Switch { name: format!("{n} switch"), got: p.caps >> i & 1 != 0 }).collect();
    switches.extend((0..2).map(|i| Switch { name: format!("Bowser key {}", i + 1), got: p.keys > i }));
    Board { got, total, groups, switches }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_board_has_120_stars_in_17_rows() {
        let b = board(&Progress::default());
        assert_eq!((b.got, b.total, b.groups.len()), (0, 120, 17));
        assert_eq!(b.groups.iter().map(|g| g.stars.len()).collect::<Vec<_>>(), [7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 5, 10]);
        assert_eq!(b.switches.len(), 5);
        assert!(b.groups.iter().flat_map(|g| &g.stars).all(|s| !s.got && !s.name.is_empty()));
    }

    #[test]
    fn flags_map_to_named_stars() {
        let mut p = Progress { keys: 1, caps: 0b101, ..Default::default() };
        p.star_flags[1] = 0b100_0001; // BoB: the summit and the 100 coins
        p.star_flags[0] = 0b10; // the upstairs Toad
        p.star_flags[19] = 0b10; // the slide's second star
        p.star_flags[24] = 1; // the aquarium
        let b = board(&p);
        assert_eq!(b.got, 5);
        let got = |g: usize| b.groups[g].stars.iter().filter(|s| s.got).map(|s| s.name.as_str()).collect::<Vec<_>>();
        assert_eq!(got(0), ["Big Bob-omb on the Summit", "100 Coins"]);
        assert_eq!(got(15), ["Toad (upstairs)"]);
        assert_eq!(got(16), ["The Princess's Secret Slide: under 21 seconds", "The Secret Aquarium"]);
        assert_eq!(b.switches.iter().filter(|s| s.got).map(|s| s.name.as_str()).collect::<Vec<_>>(), ["wing cap switch", "vanish cap switch", "Bowser key 1"]);
    }
}
