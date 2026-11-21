//! The contract between the search (`obrrt-search`) and the domain knowledge
//! (`obrrt-objectives`): what a game state looks like to an objective
//! ([`St`], [`Obj`]) and what an objective is ([`Objective`]).
//!
//! Neither side depends on the other; both depend on this crate only.

pub mod dyngeo;
pub mod features;
pub mod geo;
pub mod king;
pub mod layout;
pub mod mover;
mod read;
pub mod records;

use std::sync::Arc;

pub use read::{bhv_addr, bhv_name, check_layout, read_state};
/// "Earliest arrival near a point": the records hook ([`records`]).
pub use records::earliest_near;
pub use sm64_binds::{GamePad, buttons};

static EARLY_DIALOG_END: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// Extra work under way that [`St::wip`] adds, set by the objectives crate
/// (domain knowledge stays there; e.g. `OBRRT_TWOSTAR`'s coin count toward
/// a 100-coin star). Unset: nothing added.
static WIP_EXTRA: std::sync::OnceLock<fn(&St) -> i64> = std::sync::OnceLock::new();

/// Installs the [`St::wip`] extra (first call wins).
pub fn set_wip_extra(f: fn(&St) -> i64) {
    let _ = WIP_EXTRA.set(f);
}

/// Whether a closing dialog stops counting as a dialog once Mario plays
/// again ([`St::dialog_released`]); on by default, off for the old
/// behaviour (`OBRRT_FAST_MODAL=0`).
pub fn set_early_dialog_end(on: bool) {
    EARLY_DIALOG_END.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// An object's identity across states and replays: its behaviour and its
/// home (spawn) position, rounded; for objects without a home (0,0,0),
/// `[i32::MIN, pool slot, 0]`. Cheap to compare.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ObjId {
    pub bhv: u32,
    pub home: [i32; 3],
}

impl std::fmt::Display for ObjId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}@{},{},{}", bhv_name(self.bhv).unwrap_or("?"), self.home[0], self.home[1], self.home[2])
    }
}

/// One object in the game's object pool, as an objective sees it.
#[derive(Debug, Clone)]
pub struct Obj {
    pub id: ObjId,
    /// Index in `gObjectPool`; stable while the object is loaded.
    pub slot: u16,
    /// Address of its behaviour script; name it with [`bhv_name`].
    pub bhv: u32,
    pub pos: [f32; 3],
    /// Where it spawned (`oHome`): with `bhv`, the most stable identity an
    /// object has across states.
    pub home: [f32; 3],
    pub interact_type: u32,
    pub interact_status: u32,
    pub action: i32,
    pub health: i32,
    pub value: i32,
    /// Coins it drops when defeated (`oNumLootCoins`).
    pub loot_coins: i32,
    pub beh_params: u32,
    pub held_state: u32,
    pub intangible_timer: i32,
    /// Facing yaw (`oFaceAngleYaw`), 0..65536 with 0 facing +z.
    pub yaw: i32,
    /// The four s16 fields at 0x1AC..0x1B4 (rawData words 0x49-0x4A),
    /// behaviour-specific. The racing penguin's: oRacingPenguinFinalTextbox,
    /// oRacingPenguinMarioWon, oRacingPenguinReachedBottom,
    /// oRacingPenguinMarioCheated (include/object_fields.h).
    pub raw_1ac: [i16; 4],
}

impl Obj {
    /// A point `d` units behind the object (opposite its facing).
    pub fn behind(&self, d: f32) -> [f32; 3] {
        let a = self.yaw as f32 * std::f32::consts::TAU / 65536.0;
        [self.pos[0] - d * a.sin(), self.pos[1], self.pos[2] - d * a.cos()]
    }
}

impl Obj {
    pub fn bhv_name(&self) -> &'static str {
        bhv_name(self.bhv).unwrap_or("?")
    }

    /// [`ObjId`] as text, for pool keys.
    pub fn key(&self) -> String {
        self.id.to_string()
    }
}

/// A snapshot of the game after a frame: Mario, progress, and (when asked
/// for) the loaded objects.
#[derive(Debug, Clone)]
pub struct St {
    /// Frames since power-on.
    pub frame: u32,
    pub pos: [f32; 3],
    pub vel: [f32; 3],
    pub forward_vel: f32,
    pub yaw: i16,
    /// Mario's pitch (`faceAngle[0]`): positive is nose up. Swimming, flying
    /// and the cannon steer it; on the ground it stays 0.
    pub pitch: i16,
    /// `angleVel` (pitch, yaw, roll per frame): the turn rate swimming and
    /// flying ramp towards the stick's, and the spin when swinging Bowser.
    pub angle_vel: [i16; 3],
    /// Height of the water surface at Mario (`waterLevel`); -11000 where
    /// there is none (find_water_level, surface_collision.c).
    pub water_level: i16,
    /// The camera's yaw: the stick is relative to it.
    pub cam_yaw: i16,
    pub action: u32,
    pub mario_flags: u32,
    pub holding: bool,
    /// Pool slot of the held object.
    pub held_slot: Option<u16>,
    /// Red coins collected in this course.
    pub red_coins: i16,
    pub level: u16,
    pub area: u8,
    pub act: u8,
    pub course: i16,
    pub stars: i16,
    /// Bowser keys held or already used on their door (0 to 2).
    pub keys: u8,
    pub coins: i16,
    pub lives: i8,
    /// Mario's health, 0x000 to 0x880; `>> 8` is the number of wedges.
    pub health: i16,
    pub play_mode: i16,
    pub transition: i16,
    /// Open dialog (`gDialogID`), -1 if none; whether it ends in a yes/no
    /// choice (`gLastDialogResponse`).
    pub dialog: i16,
    pub dialog_choice: bool,
    /// Cursor of the open dialog's choice or of a menu (`gDialogLineNum`):
    /// 1 = yes / save and continue, 2 = no / save and quit, 3 = continue
    /// without saving.
    pub menu_line: i8,
    /// `gMenuMode`: -1 none, 1 pause, 2 course complete (save menu).
    pub menu_mode: i16,
    /// On the act select screen; the first act whose star is missing (the
    /// highest act it lets you load), and its cursor.
    pub act_select: bool,
    pub act_init: u8,
    pub act_cursor: i8,
    /// `gDialogBoxState` (ingame_menu.c): for a dialog 0 opening, 1 showing
    /// a page (the only state that takes a press), 2 scrolling to the next
    /// page, 3 closing; the pause and save menus reuse it (0 before the
    /// options show, 1 once they do).
    pub dialog_state: i8,
    /// The open dialog shows its last page (`gLastDialogPageStrPos == -1`):
    /// a press closes it, and a yes/no choice's cursor moves.
    pub dialog_last_page: bool,
    /// Frames the open menu has been up: `gCourseDoneMenuTimer` on the save
    /// menu, `sActSelectorMenuTimer` on the act select screen, else 0.
    pub menu_timer: i32,
    /// Read-only collision context, as the game left it after the frame
    /// (MarioState: floorHeight, ceilHeight, *floor, *wall, actionTimer).
    /// Not in the fingerprint or the dedupe key.
    ///
    /// Height of the floor under Mario (`floorHeight`); -11000 over none.
    pub floor_y: f32,
    /// Height of the ceiling above Mario (`ceilHeight`); 20000 under none.
    pub ceil_y: f32,
    /// Surface type of Mario's floor (`floor->type`, surface_terrains.h);
    /// -1 when there is no floor.
    pub floor_type: i16,
    /// The floor's normal (`floor->normal`); [0, 0, 0] when there is none.
    /// `floor_normal[1]` is the slope's cosine (1 = flat).
    pub floor_normal: [f32; 3],
    /// Pool slot of the object the floor belongs to (`floor->object`, a
    /// moving or solid object's collision), `None` for level geometry.
    pub floor_obj: Option<u16>,
    /// That object's `oVelY` and `oPosY`. Platforms that rise by setting
    /// oPosY directly leave oVelY at 0; the rise per frame is then the
    /// difference of `floor_obj_y` (or `floor_y`) between frames.
    pub floor_obj_vy: Option<f32>,
    pub floor_obj_y: Option<f32>,
    /// `actionTimer`: frames in the current action for actions that count
    /// it (e.g. the long-jump landing's A window).
    pub action_timer: u16,
    /// Frames left on Mario's cap (`MARIO_capTimer`, 0 without a timed cap): the wing/metal/vanish cap's window.
    pub cap_timer: u16,
    /// Normal of the wall Mario last collided with (`m->wall`), if any. The
    /// game sets it in its ground and air steps and doesn't always clear it,
    /// so it can be a frame stale.
    pub wall_normal: Option<[f32; 3]>,
    /// Star flags per course (index course - 1): bits 0-5 the acts' stars,
    /// bit 6 the 100-coin star.
    pub course_stars: [u8; 25],
    /// The save file's flags word (`SaveFile.flags`): caps unlocked, keys,
    /// doors opened, and similar one-way unlocks.
    pub save_flags: u32,
    pub objects: Option<Arc<Vec<Obj>>>,
}

impl St {
    pub fn wedges(&self) -> i32 {
        (self.health as i32) >> 8
    }

    pub fn alive(&self) -> bool {
        self.wedges() >= 1
    }

    /// (keys, stars): compared lexicographically, the run's notion of
    /// progress.
    pub fn progress(&self) -> (u8, i16) {
        (self.keys, self.stars)
    }

    /// Which progress, not just how much: a hash of every per-course flag
    /// byte (stars and cannons) and the save file's flags word (keys, caps,
    /// doors). Two states with the same `progress()` count but different
    /// collected sets differ here.
    pub fn progress_set(&self) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        std::hash::Hash::hash(&(self.course_stars, self.save_flags), &mut h);
        std::hash::Hasher::finish(&h)
    }

    /// What has been collected, as a bitset that only ever gains bits: one
    /// byte per course (its star flags, without the cannon bit) and one for
    /// the castle's own stars (flags word bits 24-30). A state whose bitset
    /// is a superset of another's has collected at least everything it has.
    pub fn collected(&self) -> [u8; 26] {
        let mut out = [0u8; 26];
        for (o, c) in out.iter_mut().zip(&self.course_stars) {
            *o = c & 0x7f;
        }
        out[25] = ((self.save_flags >> 24) & 0x7f) as u8;
        out
    }

    pub fn zone(&self) -> (u16, u8) {
        (self.level, self.area)
    }

    pub fn action_has(&self, flag: u32) -> bool {
        self.action & flag != 0
    }

    /// Whether the player is in control: normal play, not in a cutscene,
    /// dialog, menu or warp. Otherwise inputs mostly just advance text and menus.
    pub fn controllable(&self) -> bool {
        use crate::layout::{ACT_GROUP_CUTSCENE, ACT_GROUP_MASK};
        self.play_mode == 0 && self.action != 0 && self.action & ACT_GROUP_MASK != ACT_GROUP_CUTSCENE && !self.modal()
    }

    /// Changes to the level that stay put and that the counts don't show:
    /// how far wooden posts are pounded in, in 40-unit steps (the Chomp's
    /// post frees him after 3 pounds, and he breaks the gate to BoB's star 6;
    /// bhv_wooden_post_update, chain_chomp.inc.c). Without it every pound
    /// looks like a repeat of the state before it (in the fingerprint), and
    /// the states after a pound share their b cell with every state at the
    /// post (in the engine's frontier index).
    pub fn level_changes(&self) -> i64 {
        self.objects()
            .iter()
            .filter(|o| o.bhv_name() == "bhvWoodenPost")
            .map(|o| ((o.home[1] - o.pos[1]) / 40.0).round().max(0.0) as i64)
            .sum()
    }

    /// Work under way in the level that the collected counts don't show
    /// (for OBRRT_KEEP_WIP): boss damage dealt (each boss in the object list
    /// below its starting health: King Bob-omb, Whomp King, the Big Boos 3;
    /// Wiggler, Eyerok's hands 4; a boss at 0 counts its full health), a
    /// boss's star spawned and not yet collected (3), red coins this visit,
    /// and `level_changes` (posts pounded). Larger is further along; only
    /// compared within a level.
    pub fn wip(&self) -> i64 {
        let mut w = self.red_coins.max(0) as i64 + self.level_changes();
        // Cap switches pressed (save flag bits 1-3): pressing one is work done
        // that neither the star count nor the objects show.
        w += (self.save_flags & 0xE).count_ones() as i64;
        if let Some(f) = WIP_EXTRA.get() {
            w += f(self);
        }
        for o in self.objects() {
            let max = match o.bhv_name() {
                "bhvKingBobomb" | "bhvWhompKingBoss" | "bhvGhostHuntBigBoo" | "bhvBalconyBigBoo" | "bhvMerryGoRoundBigBoo" => 3,
                "bhvWigglerHead" | "bhvEyerokHand" => 4,
                "bhvStarSpawnCoordinates" | "bhvSpawnedStar" | "bhvSpawnedStarNoLevelExit" => {
                    w += 3;
                    continue;
                }
                // A treasure chest opened in order (treasure_chest.inc.c: the
                // bottom's action 1; a wrong one resets the set): the JRB ship
                // and cave and DDD's chests, work no count shows (2026-11-09).
                "bhvTreasureChestBottom" => {
                    w += (o.action == 1) as i64;
                    continue;
                }
                _ => continue,
            };
            w += (max - o.health.clamp(0, max)) as i64;
        }
        w
    }

    /// Progress a state's place and counts don't show, for telling states
    /// apart: red coins, holding something, and the interactable objects
    /// loaded (how many, and their total health: an enemy beaten, a coin or
    /// box taken, a boss hit). Two states at the same spot with different
    /// fingerprints are different states.
    pub fn fingerprint(&self) -> u64 {
        let (mut n, mut health) = (0u64, 0i64);
        for o in self.objects() {
            if o.interact_type != 0 {
                n += 1;
                health += o.health.clamp(0, 255) as i64;
            }
        }
        let sunk = self.level_changes();
        let mut h = std::collections::hash_map::DefaultHasher::new();
        std::hash::Hash::hash(&(self.red_coins, self.holding, n, health, self.objects.is_some(), sunk), &mut h);
        std::hash::Hasher::finish(&h)
    }

    /// In a dialog that still holds Mario. With [`set_early_dialog_end`] on
    /// (the default), a dialog that is closing while Mario already plays
    /// ([`St::dialog_released`]) no longer counts.
    pub fn in_dialog(&self) -> bool {
        self.dialog >= 0 && !self.dialog_released()
    }

    /// The open dialog is closing (`gDialogBoxState` 3) and Mario already
    /// plays: normal play mode (the box's first closing frame ends a level's
    /// `level_set_transition(-1)` freeze, ingame_menu.c:1768-1777) and an
    /// action outside the cutscene group (the dialog actions wait for
    /// gDialogID to clear). gDialogID itself stays set for ~8 more frames.
    /// Always false with [`set_early_dialog_end`] off.
    pub fn dialog_released(&self) -> bool {
        use crate::layout::{ACT_GROUP_CUTSCENE, ACT_GROUP_MASK};
        EARLY_DIALOG_END.load(std::sync::atomic::Ordering::Relaxed)
            && self.dialog >= 0
            && self.dialog_state == 3
            && self.play_mode == 0
            && self.action != 0
            && self.action & ACT_GROUP_MASK != ACT_GROUP_CUTSCENE
    }

    pub fn in_save_menu(&self) -> bool {
        self.menu_mode == crate::layout::MENU_MODE_RENDER_COURSE_COMPLETE_SCREEN as i16
    }

    /// In a dialog or menu whose options are objectives of their own (see
    /// [`Objective::modal`]): a dialog, the save menu, the act select screen.
    pub fn modal(&self) -> bool {
        self.in_dialog() || self.in_save_menu() || self.act_select
    }

    /// Whether star `index` (0-5 acts, 6 the 100-coin star) of `course` is
    /// collected.
    pub fn has_star(&self, course: i16, index: u8) -> bool {
        (1..=25).contains(&course) && self.course_stars[course as usize - 1] & (1 << index) != 0
    }

    /// Whether every star of `course` is collected (main courses have 7,
    /// Bowser courses 1, the Princess's Secret Slide 2, other secrets 1),
    /// and, in a cap course (20 metal, 21 wing, 22 vanish), its switch is
    /// pressed (save_file.h SAVE_FLAG_HAVE_METAL_CAP 0x4, _WING_CAP 0x2,
    /// _VANISH_CAP 0x8). Without the switch a cap course counted as done
    /// with its star, and the engine plans nothing from the states of a done
    /// course (goal cells, the new-eligible and early-arrival queues): the
    /// 26-star route of campaign t1 left the wing cap tower with the star
    /// and the switch unpressed (2026-11-10).
    pub fn course_done(&self, course: i16) -> bool {
        let all = match course {
            1..=15 => 0x7F,
            19 => 0x03,
            16..=24 => 0x01,
            _ => return false,
        };
        let cap = match course {
            20 => 0x4,
            21 => 0x2,
            22 => 0x8,
            _ => 0,
        };
        self.save_flags & cap == cap && self.course_stars[course as usize - 1] & all == all
    }

    pub fn objects(&self) -> &[Obj] {
        self.objects.as_deref().map_or(&[], |v| v.as_slice())
    }

    pub fn find(&self, id: ObjId) -> Option<&Obj> {
        self.objects().iter().find(|o| o.id == id)
    }

    /// The object Mario is holding, if any.
    pub fn held(&self) -> Option<&Obj> {
        self.held_slot.and_then(|slot| self.objects().iter().find(|o| o.slot == slot))
    }

    pub fn dist(&self, p: [f32; 3]) -> f32 {
        dist(self.pos, p)
    }

    /// Where the state counts as being for a climb's costs (search/src/road.rs,
    /// `Road::Climb`): its position, or for an airborne state the floor under
    /// it. A wall-kick chain's peak is no height gained unless there is
    /// something to land on: in SSL's pyramid the corner cell 1100 units above
    /// the pole's top made the pole's top the "nearest" state to the star, and
    /// every state further along the way up ranked behind it (camp-sslcp-0001,
    /// 2026-11-10). Flying and cannon shots keep their own height.
    pub fn footing(&self) -> [f32; 3] {
        let air = self.action & 0x1C0 == 0x080 && self.action != layout::ACT_FLYING && self.action != layout::ACT_SHOT_FROM_CANNON;
        if air && self.floor_y > -10000.0 && self.floor_y < self.pos[1] {
            [self.pos[0], self.floor_y, self.pos[2]]
        } else {
            self.pos
        }
    }
    pub fn hdist(&self, p: [f32; 3]) -> f32 {
        ((self.pos[0] - p[0]).powi(2) + (self.pos[2] - p[2]).powi(2)).sqrt()
    }
}

/// The course a level belongs to (gLevelToCourseNumTable), 0 for the castle
/// and anything that isn't a course.
pub fn course_of_level(level: u32) -> i16 {
    use crate::layout::*;
    const COURSES: &[(u32, i16)] = &[
        (LEVEL_BOB, 1), (LEVEL_WF, 2), (LEVEL_JRB, 3), (LEVEL_CCM, 4), (LEVEL_BBH, 5), (LEVEL_HMC, 6),
        (LEVEL_LLL, 7), (LEVEL_SSL, 8), (LEVEL_DDD, 9), (LEVEL_SL, 10), (LEVEL_WDW, 11), (LEVEL_TTM, 12),
        (LEVEL_THI, 13), (LEVEL_TTC, 14), (LEVEL_RR, 15), (LEVEL_BITDW, 16), (LEVEL_BITFS, 17), (LEVEL_BITS, 18),
        (LEVEL_PSS, 19), (LEVEL_COTMC, 20), (LEVEL_TOTWC, 21), (LEVEL_VCUTM, 22), (LEVEL_WMOTR, 23), (LEVEL_SA, 24),
    ];
    COURSES.iter().find(|c| c.0 == level).map_or(0, |c| c.1)
}

pub fn dist(a: [f32; 3], b: [f32; 3]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

/// How precise an objective's inputs need to be, which sets how much GA it
/// gets: `Precise` runs more controller generations per commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Precision {
    /// Exploration: reaching a place or touching something is enough, and a
    /// first success is about as good as a refined one. Gets the base GA
    /// (1x generations, stopping one generation after a full success).
    Explore,
    /// A narrow window (hitting a boss, grabbing, a star, a race, a cannon
    /// shot): evolution past the first success pays. Gets the full GA
    /// (scaled generations, no early stop).
    Precise,
}

/// Which states along an attempt's winning rollout become dense nodes
/// (`OBRRT_ADD=winner_all|all` with `OBRRT_DENSE_EVERY=obj`), chosen by the
/// objective each stretch of the rollout was working on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DenseKeep {
    /// The search's default (`OBRRT_DENSE_EVERY=obj:<n>`, `OBRRT_DENSE_AIR`).
    Global,
    /// None between the rollout's end and its met goals.
    EndOnly,
    /// Every `n`-th frame (1 = all), and mid-air states only with `air`
    /// (else just take-off frames).
    Every { n: u32, air: bool },
}

/// v1's per-objective GA budget, kept as a hint for the rolling-horizon
/// controller ([`Objective::ga_budget`]): `population` is its population,
/// `horizon` the most frames it commits, and `population * generations *
/// horizon` (times the search's generation scale) caps the frames an attempt
/// simulates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GaBudget {
    pub population: usize,
    pub generations: usize,
    pub horizon: usize,
}

/// A fixed way to play an objective ([`Objective::fixed`]).
#[derive(Debug, Clone)]
pub enum Budget {
    /// A fixed input sequence (e.g. pausing and choosing Exit Course).
    Macro(Vec<GamePad>),
    /// Closed loop: each frame's input from the states so far (e.g. press
    /// on the first frame a menu takes input, once its cursor is right), for
    /// at most `limit` frames or until the objective is met. Played once,
    /// like a macro.
    Policy { limit: usize, policy: PadPolicy },
}

impl Budget {
    pub fn frames(&self) -> usize {
        match self {
            Budget::Macro(pads) => pads.len(),
            Budget::Policy { limit, .. } => *limit,
        }
    }
}

/// The rule of a [`Budget::Policy`]: given the trajectory so far (the start
/// state, then one state per frame played) and the inputs played, the next
/// frame's input.
#[derive(Clone)]
pub struct PadPolicy(Arc<dyn Fn(&[St], &[GamePad]) -> GamePad + Send + Sync>);

impl PadPolicy {
    pub fn new(f: impl Fn(&[St], &[GamePad]) -> GamePad + Send + Sync + 'static) -> Self {
        PadPolicy(Arc::new(f))
    }

    pub fn pad(&self, traj: &[St], played: &[GamePad]) -> GamePad {
        (self.0)(traj, played)
    }
}

/// How a steering target must be reached ([`Objective::arrival`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Arrival {
    /// Touching it or passing through it counts (a progress point, a coin,
    /// a painting, a warp): overshooting costs nothing.
    #[default]
    Pass,
    /// Mario must stop, stand or interact at the spot (talk, grab, press,
    /// board a platform): overshooting means coming back.
    Stop,
    /// A door: opens only to walking within reach of it (land short, walk
    /// in).
    Door,
}

/// The rule of [`Objective::pad_overlay`]. A keyed rule also says which
/// frames are "key": where the genome's input it keeps decides the outcome
/// (e.g. the stick on an exactly timed press). The GA focuses some of its
/// mutations on those frames' sticks.
#[derive(Clone)]
pub struct PadOverlay(Arc<dyn Fn(&[St], &[GamePad], GamePad) -> (GamePad, bool) + Send + Sync>);

impl PadOverlay {
    pub fn new(f: impl Fn(&[St], &[GamePad], GamePad) -> GamePad + Send + Sync + 'static) -> Self {
        PadOverlay(Arc::new(move |t, p, g| (f(t, p, g), false)))
    }

    pub fn keyed(f: impl Fn(&[St], &[GamePad], GamePad) -> (GamePad, bool) + Send + Sync + 'static) -> Self {
        PadOverlay(Arc::new(f))
    }

    pub fn pad(&self, traj: &[St], played: &[GamePad], genome: GamePad) -> GamePad {
        (self.0)(traj, played, genome).0
    }

    /// The pad, and whether this frame is key.
    pub fn pad_keyed(&self, traj: &[St], played: &[GamePad], genome: GamePad) -> (GamePad, bool) {
        (self.0)(traj, played, genome)
    }
}

impl std::fmt::Debug for PadPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PadPolicy")
    }
}

/// An objective's estimate of the frames left until it is met, from one
/// state (v2: the one value function; see GOALS.md).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Eta {
    /// Met at this state.
    Done,
    /// Estimated frames to go (≥ 0), at a reference pace ([`PACE`]).
    Frames(f64),
    /// Can't be met from here (wrong zone, object gone, deadline passed).
    Lost,
}

impl Eta {
    /// Frames, with `Done` = 0 and `Lost` = infinity.
    pub fn frames(self) -> f64 {
        match self {
            Eta::Done => 0.0,
            Eta::Frames(f) => f.max(0.0),
            Eta::Lost => f64::INFINITY,
        }
    }
    pub fn done(self) -> bool {
        self == Eta::Done
    }
}

/// Reference pace (units per frame) distances are turned into frames by.
pub const PACE: f64 = 30.0;

/// Default start softmax temperature in ETA frames: a start 60 frames
/// further away is e times less likely.
pub const START_TEMPERATURE: f64 = 60.0;

/// Frames for a distance at [`PACE`].
pub fn frames_for(d: f32) -> f64 {
    d as f64 / PACE
}

/// Per-rollout scratch of an order-dependent objective (latches,
/// accumulators, the frame something first happened): reset at a start,
/// folded through [`Objective::eta`] frame by frame, in order.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Mem {
    pub f: [f64; 4],
    pub flags: u32,
}

/// Folds `obj.eta` over `traj` (index 0 the start): the first index met and
/// the ETA at each state.
pub fn fold(obj: &dyn Objective, traj: &[St]) -> (Option<usize>, Vec<Eta>) {
    let mut mem = Mem::default();
    let mut out = Vec::with_capacity(traj.len());
    let mut met = None;
    for (i, s) in traj.iter().enumerate() {
        let e = obj.eta(s, &traj[0], &mut mem);
        if met.is_none() && e.done() {
            met = Some(i);
        }
        out.push(e);
    }
    (met, out)
}

/// The first index of `traj` at which `obj` is met (fold of `eta`).
pub fn met_at(obj: &dyn Objective, traj: &[St]) -> Option<usize> {
    let mut mem = Mem::default();
    traj.iter().position(|s| obj.eta(s, &traj[0], &mut mem).done())
}

/// `s` as a start: its ETA with itself as the start and fresh scratch, in
/// frames (`Lost` = infinity). Ranks starts (lower is better).
pub fn start_eta(obj: &dyn Objective, s: &St) -> f64 {
    obj.eta(s, s, &mut Mem::default()).frames()
}

/// `s`'s start rank for `obj`: [`start_eta`] plus [`Objective::start_bias`]
/// (frames, lower is better). The engine and schemapick rank starts (and
/// judge a start's improvement) by it; eligibility cutoffs, the arrival
/// estimate and calibration keep the pure [`start_eta`]. With a zero bias it
/// is exactly `start_eta`. (With OBRRT_ETA_LEARN the engine ranks by its
/// kind's learned c × start_eta + bias and estimates arrival with c ×
/// start_eta; cutoffs stay raw, search/src/etalearn.rs.)
pub fn start_rank(obj: &dyn Objective, s: &St) -> f64 {
    let h = start_eta(obj, s);
    let b = obj.start_bias(s);
    if b == 0.0 { h } else { h + b }
}

/// v1's heuristic unit (400 units of distance, `dist / 400`) in frames at
/// [`PACE`]: what a v1 non-distance ranking term of 1.0 is worth when ported
/// to a [`Objective::start_bias`] (engine.rs `IMPROVE_FRAMES` = 7 is v1's 0.5
/// the same way).
pub const V1_UNIT_FRAMES: f64 = 400.0 / PACE;

/// A [`Objective::start_bias`] for "the earliest start first" (v1's
/// `frame / 30` for dialogs and menus): one frame of rank per frame of game
/// time, since a start reached later is done that much later.
pub fn earliest_bias(s: &St) -> f64 {
    s.frame as f64
}

/// One thing to achieve: `Object::Interaction(params)`.
///
/// v2: an objective says *what* (eligibility and a per-state ETA in frames);
/// the search's rolling-horizon controller decides *how* (its fitness is the
/// discounted ETA decrease, see search/src/ga.rs). The methods after `eta`
/// are optional hints for the controller and the search.
pub trait Objective: Send + Sync {
    /// Type name for diagnostics and weights, e.g. `"Coin::Collect"`.
    fn kind(&self) -> &'static str;
    /// Identity in the pool: two objectives with the same key are the same.
    fn key(&self) -> String;
    /// Importance; the pool samples by it (times the search's learned
    /// multipliers and failure decay).
    fn obj_weight(&self, b: &St) -> f64;
    /// Whether `s` may start an attempt.
    fn base_criteria(&self, s: &St) -> bool;
    /// Frames left until met, from state `s` of a rollout that began at
    /// `start`. Called once per frame in order with the same `mem` (fresh at
    /// the start), so order-dependent objectives keep their latches there.
    /// Must be honest frames (distances / [`PACE`], phases summed), the same
    /// unit for every objective.
    fn eta(&self, s: &St, start: &St, mem: &mut Mem) -> Eta;

    // ---- hints ----

    /// A fixed way to play it (menus, macros) instead of the controller.
    fn fixed(&self) -> Option<Budget> {
        None
    }
    /// v1's GA budget for this objective (population, generations,
    /// horizon); None = the controller's defaults.
    fn ga_budget(&self) -> Option<GaBudget> {
        None
    }
    /// How precise this objective's inputs need to be; `Precise` gets more
    /// controller generations per commit. Defaults to `Explore`.
    fn precision(&self) -> Precision {
        Precision::Explore
    }
    /// Kinship limit for the universal base criteria: the start must share an
    /// ancestor with `b` at most this many generations up.
    fn kinship(&self) -> u32 {
        6
    }
    /// Dense adds with `OBRRT_DENSE_EVERY=obj`: which of the states its
    /// stretch of a winning rollout passes through become nodes.
    fn dense_keep(&self) -> DenseKeep {
        DenseKeep::Global
    }
    /// Start ranking only (frames, default 0): a preference among starts
    /// that isn't a distance to go (v1's non-distance `base_heuristic`
    /// terms: earliest frame, most stars and coins, ...). Added to
    /// [`start_eta`] by [`start_rank`]; never seen by the controller's
    /// fitness, eligibility cutoffs or the arrival estimate.
    fn start_bias(&self, _s: &St) -> f64 {
        0.0
    }
    /// Frames the goal still costs after the `Done` state, which the
    /// controller's fitness subtracts on that frame (v1's non-default
    /// `success_heuristic`: a star grab's fall to the dance, a fast exit's
    /// warp countdown). `mem` is the stage's scratch as `eta` left it on the
    /// Done frame. Default 0.
    fn done_frames(&self, _s: &St, _mem: &Mem) -> f64 {
        0.0
    }
    /// Softmax temperature, in frames of ETA, when picking the start.
    fn temperature(&self) -> f64 {
        crate::START_TEMPERATURE
    }
    /// Whether trajectory states need the object list (costs time per frame).
    fn needs_objects(&self) -> bool {
        false
    }
    /// A point the GA's controller-seeded genomes steer towards, if any.
    /// Asked every frame, so it can follow a moving object.
    fn steer_target(&self, _s: &St) -> Option<[f32; 3]> {
        None
    }
    /// How much a success is worth to the run (domain knowledge): counted in
    /// this kind's yield when the success adds a state. A boss hit or a red
    /// coin is progress even though it doesn't change stars or keys.
    fn success_value(&self) -> f64 {
        0.0
    }
    /// A diagnostics tag (`"<block>/<name>"`): the search counts this
    /// objective's attempts, successes and added states under it, in
    /// summary.json's `<block>.attempts.<name>`. Pure bookkeeping.
    fn diag_tag(&self) -> Option<String> {
        None
    }
    /// The search's verdict on one attempt of this objective, at merge (in
    /// merge order): success, the segment's played frames and its end
    /// state's forward speed. Pure bookkeeping (diagnostics registries).
    fn note_result(&self, _success: bool, _frames: usize, _end_fv: Option<f32>) {}
    /// A button the controller presses on alternate frames instead of its
    /// usual "A while the target is higher" (e.g. B to grab when close).
    fn steer_button(&self, _s: &St) -> u16 {
        0
    }
    /// How the steering target must be reached (domain knowledge for a
    /// movement library, [`mover::Mover`]): passing through or past it is
    /// fine ([`Arrival::Pass`], the default), Mario must stop there
    /// ([`Arrival::Stop`]), or it is a door, which opens only to walking
    /// ([`Arrival::Door`]). Only a registered mover reads it.
    fn arrival(&self, _s: &St) -> Arrival {
        Arrival::Pass
    }
    /// A per-frame rule over the GA's inputs: given the trajectory so far,
    /// the inputs played and the genome's (or the steering controller's)
    /// pad for this frame, the pad actually played. Lets an objective fix
    /// fields that need exact timing (e.g. buttons) while the GA evolves the
    /// rest. The played pad is what the genome keeps. Only GA rollouts use it.
    fn pad_overlay(&self) -> Option<PadOverlay> {
        None
    }
    /// A dialog or menu option: attempted only from modal states
    /// ([`St::modal`]), where nothing else is. In a modal state these are
    /// all that can be sampled (each option is tried as soon as the state
    /// is added); outside one they have no weight.
    fn modal(&self) -> bool {
        false
    }
    /// Whether this objective can still be attempted at all in `s` (its
    /// object exists); the pool drops objectives that are dead everywhere.
    fn exists_in(&self, _s: &St) -> bool {
        true
    }
}

/// Makes objectives from a base state `b` (which has its objects).
pub trait Generator: Send + Sync {
    fn generate(&self, b: &St, rng: &mut rand::rngs::SmallRng) -> Vec<Arc<dyn Objective>>;
    /// Whether generation reads [`records`] (the search then keeps and
    /// installs them; otherwise it doesn't build them at all).
    fn wants_records(&self) -> bool {
        false
    }
    /// Extra summary.json blocks, as (name, JSON text) (diagnostics only).
    fn summary_blocks(&self) -> Vec<(String, String)> {
        Vec::new()
    }
    /// Writes the generator's own report files into the run's `out_dir`
    /// (diagnostics only; called with each progress report).
    fn report(&self, _out_dir: &str) {}
}

/// Hooks the search calls for diagnostics registries (BLJ spot registry,
/// `OBRRT_BLJ_SPOTS`): unset, nothing happens.
pub mod hooks {
    use std::sync::OnceLock;

    use crate::St;

    type NodeHook = fn(u32, u32, &St);
    static NODE: OnceLock<NodeHook> = OnceLock::new();

    /// Sets the node hook (first call wins): called by the tree for every
    /// added node, in merge order, with (id, parent id, state).
    pub fn set_node_hook(f: NodeHook) {
        let _ = NODE.set(f);
    }

    pub fn node_added(id: u32, parent: u32, st: &St) {
        if let Some(f) = NODE.get() {
            f(id, parent, st);
        }
    }
}
