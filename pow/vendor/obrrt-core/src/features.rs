//! Raw state features for the learned backbone (`backbone/`, sm64_AI_backbone.md):
//! one game state as fixed columns of Mario, collision, camera, progress,
//! context and UI fields, plus the nearest non-cosmetic objects and the
//! nearby object-owned (dynamic) collision triangles. Static level collision
//! is dumped once per zone ([`zone_geometry`]); the model picks the nearest
//! triangles itself.
//!
//! Only raw values: scaling, symlog, Fourier features and sin/cos of angles
//! happen inside the model, so the ONNX export takes exactly these arrays.
//! Read-only, like [`crate::read_state`]; nothing here changes the game.

use crate::layout::*;
use crate::{St, geo};

/// Hard caps (nearest by distance to Mario when over).
pub const MAX_OBJ: usize = 64;
pub const MAX_DSURF: usize = 32;
/// Water / gas boxes kept, nearest by horizontal distance.
pub const MAX_WATER: usize = 4;

/// Integer columns of the global row (exact: flags, ids, counters).
pub const INT_COLS: &[&str] = &[
    "action", "prev_action", "action_state", "action_arg", "action_timer", "mario_flags", "health",
    "cap_timer", "invinc_timer", "squish_timer", "hurt_counter", "heal_counter", "wall_kick_timer",
    "double_jump_timer", "frames_since_a", "frames_since_b", "yaw", "pitch", "angvel_x", "angvel_y",
    "angvel_z", "twirl_yaw", "slide_yaw", "intended_yaw", "holding", "held_slot", "floor_type",
    "floor_is_obj", "has_wall", "water_level", "cam_yaw", "cam_mode", "cam_cutscene", "cam_selection",
    "cam_move_flags", "cam_c_side_yaw", "level", "area", "act", "course", "play_mode", "transition",
    "stars", "keys", "coins", "red_coins", "lives", "save_flags", "dialog", "dialog_state",
    "dialog_last_page", "dialog_choice", "menu_line", "menu_mode", "act_select", "act_init", "act_cursor",
    "menu_timer", "rng_seed", "global_timer", "level_changes", "wip", "collided_types", "controllable",
    "cs0", "cs1", "cs2", "cs3", "cs4", "cs5", "cs6", "cs7", "cs8", "cs9", "cs10", "cs11", "cs12", "cs13",
    "cs14", "cs15", "cs16", "cs17", "cs18", "cs19", "cs20", "cs21", "cs22", "cs23", "cs24",
];

/// Float columns of the global row (absolute world units).
pub const FLT_COLS: &[&str] = &[
    "pos_x", "pos_y", "pos_z", "vel_x", "vel_y", "vel_z", "fwd_vel", "slide_vel_x", "slide_vel_z",
    "peak_height", "quicksand_depth", "intended_mag", "floor_y", "ceil_y", "floor_nx", "floor_ny",
    "floor_nz", "wall_nx", "wall_ny", "wall_nz", "floor_obj_vy", "floor_obj_y",
    "w0_id", "w0_lox", "w0_loz", "w0_hix", "w0_hiz", "w0_y",
    "w1_id", "w1_lox", "w1_loz", "w1_hix", "w1_hiz", "w1_y",
    "w2_id", "w2_lox", "w2_loz", "w2_hix", "w2_hiz", "w2_y",
    "w3_id", "w3_lox", "w3_loz", "w3_hix", "w3_hiz", "w3_y",
];

pub const OBJ_INT_COLS: &[&str] = &[
    "slot", "bhv", "interact_type", "interact_status", "action", "sub_action", "timer", "health",
    "value", "loot_coins", "beh_params", "held_state", "intangible_timer", "face_yaw", "move_yaw",
    "angvel_yaw", "active_flags", "raw0", "raw1", "raw2", "raw3", "is_held", "is_floor",
];

pub const OBJ_FLT_COLS: &[&str] = &[
    "pos_x", "pos_y", "pos_z", "home_x", "home_y", "home_z", "vel_x", "vel_y", "vel_z", "fwd_vel",
    "hitbox_r", "hitbox_h", "hurtbox_r", "hurtbox_h",
];

/// A collision triangle: vertices, normal, type, owner (pool slot, -1 for
/// level geometry) and the owner's velocity.
pub const TRI_COLS: &[&str] = &[
    "v0x", "v0y", "v0z", "v1x", "v1y", "v1z", "v2x", "v2y", "v2z", "nx", "ny", "nz", "kind", "list",
    "owner", "ovx", "ovy", "ovz",
];

/// One state's features.
#[derive(Debug, Clone, Default)]
pub struct Features {
    pub ints: Vec<i32>,
    pub flts: Vec<f32>,
    pub obj_ints: Vec<i32>,
    pub obj_flts: Vec<f32>,
    pub n_obj: usize,
    pub tris: Vec<f32>,
    pub n_tri: usize,
}

struct Mem<'a>(&'a [u8]);

impl Mem<'_> {
    fn u32(&self, a: usize) -> u32 {
        self.0.get(a..a + 4).map_or(0, |b| u32::from_le_bytes(b.try_into().unwrap()))
    }
    fn i32(&self, a: usize) -> i32 {
        self.u32(a) as i32
    }
    fn i16(&self, a: usize) -> i16 {
        self.0.get(a..a + 2).map_or(0, |b| i16::from_le_bytes(b.try_into().unwrap()))
    }
    fn u16(&self, a: usize) -> u16 {
        self.i16(a) as u16
    }
    fn u8(&self, a: usize) -> u8 {
        self.0.get(a).copied().unwrap_or(0)
    }
    fn f32(&self, a: usize) -> f32 {
        f32::from_bits(self.u32(a))
    }
}

const MARIO: usize = ADDR_gMarioStates as usize;
const POOL: usize = ADDR_gObjectPool as usize;

/// Behaviours that only cost tokens: particles, dust, sparkles, ambient
/// animals and decorations (sm64_AI_backbone.md 2.4). Matched by name.
pub fn cosmetic(name: &str) -> bool {
    const PARTS: &[&str] = &[
        "Particle", "Sparkle", "Dust", "Smoke", "Butterfly", "Bird", "Fish", "Bubble", "Leaf", "Sound",
        "Ripple", "Splash", "Droplet", "Twinkle", "WaterWave", "WaveTrail", "SkeeterWave", "Seaweed",
        "FlagWaving", "KoopaFlag", "WhitePuff", "Mist", "TreeSnow", "Triangle", "Shadow", "BeginningLakitu",
        "BeginningPeach", "EndBirds", "IntroScene",
    ];
    // Air bubbles refill health underwater; Mr. I's particle is his projectile.
    const KEEP: &[&str] = &["bhvWaterAirBubble", "bhvBubbleMaybe", "bhvMrIParticle"];
    name == "bhvMario" || !KEEP.contains(&name) && PARTS.iter().any(|p| name.contains(p))
}

/// Index of a behaviour in [`BEHAVIOURS`] (the model's embedding row), or
/// `BEHAVIOURS.len()` for an unknown script.
pub fn bhv_index(addr: u32) -> i32 {
    BEHAVIOURS.binary_search_by_key(&addr, |&(a, _)| a).map_or(BEHAVIOURS.len() as i32, |i| i as i32)
}

/// Reads `st`'s features from memory `m` (`st` read from the same frame,
/// with objects).
pub fn read_features(m: &[u8], st: &St) -> Features {
    let mem = Mem(m);
    let mut f = Features::default();
    let mario_obj = mem.u32(MARIO + MARIO_marioObj) as usize;
    let floor = mem.u32(MARIO + MARIO_floor) as usize;
    let floor_owner = if floor != 0 { mem.u32(floor + SURFACE_object) as usize } else { 0 };
    let cam = mem.u32(ADDR_gCamera as usize) as usize;
    let (cam_mode, cam_cut) = if cam != 0 { (mem.u8(cam + CAMERA_mode) as i32, mem.u8(cam + CAMERA_cutscene) as i32) } else { (-1, -1) };

    let i = &mut f.ints;
    i.push(st.action as i32);
    i.push(mem.u32(MARIO + MARIO_prevAction) as i32);
    i.push(mem.u16(MARIO + MARIO_actionState) as i32);
    i.push(mem.u32(MARIO + MARIO_actionArg) as i32);
    i.push(st.action_timer as i32);
    i.push(st.mario_flags as i32);
    i.push(st.health as i32);
    i.push(mem.u16(MARIO + MARIO_capTimer) as i32);
    i.push(mem.i16(MARIO + MARIO_invincTimer) as i32);
    i.push(mem.u8(MARIO + MARIO_squishTimer) as i32);
    i.push(mem.u8(MARIO + MARIO_hurtCounter) as i32);
    i.push(mem.u8(MARIO + MARIO_healCounter) as i32);
    i.push(mem.u8(MARIO + MARIO_wallKickTimer) as i32);
    i.push(mem.u8(MARIO + MARIO_doubleJumpTimer) as i32);
    i.push(mem.u8(MARIO + MARIO_framesSinceA) as i32);
    i.push(mem.u8(MARIO + MARIO_framesSinceB) as i32);
    i.push(st.yaw as i32);
    i.push(st.pitch as i32);
    i.extend(st.angle_vel.map(|v| v as i32));
    i.push(mem.i16(MARIO + MARIO_twirlYaw) as i32);
    i.push(mem.i16(MARIO + MARIO_slideYaw) as i32);
    i.push(mem.i16(MARIO + MARIO_intendedYaw) as i32);
    i.push(st.holding as i32);
    i.push(st.held_slot.map_or(-1, |s| s as i32));
    i.push(st.floor_type as i32);
    i.push(st.floor_obj.is_some() as i32);
    i.push(st.wall_normal.is_some() as i32);
    i.push(st.water_level as i32);
    i.push(st.cam_yaw as i32);
    i.push(cam_mode);
    i.push(cam_cut);
    i.push(mem.u16(ADDR_sSelectionFlags as usize) as i32);
    i.push(mem.u16(ADDR_gCameraMovementFlags as usize) as i32);
    i.push(mem.i16(ADDR_sCSideButtonYaw as usize) as i32);
    i.push(st.level as i32);
    i.push(st.area as i32);
    i.push(st.act as i32);
    i.push(st.course as i32);
    i.push(st.play_mode as i32);
    i.push(st.transition as i32);
    i.push(st.stars as i32);
    i.push(st.keys as i32);
    i.push(st.coins as i32);
    i.push(st.red_coins as i32);
    i.push(st.lives as i32);
    i.push(st.save_flags as i32);
    i.push(st.dialog as i32);
    i.push(st.dialog_state as i32);
    i.push(st.dialog_last_page as i32);
    i.push(st.dialog_choice as i32);
    i.push(st.menu_line as i32);
    i.push(st.menu_mode as i32);
    i.push(st.act_select as i32);
    i.push(st.act_init as i32);
    i.push(st.act_cursor as i32);
    i.push(st.menu_timer);
    i.push(mem.u16(ADDR_gRandomSeed16 as usize) as i32);
    i.push(mem.u32(ADDR_gGlobalTimer as usize) as i32);
    i.push(st.level_changes() as i32);
    i.push(st.wip() as i32);
    i.push(mem.u32(MARIO + MARIO_collidedObjInteractTypes) as i32);
    i.push(st.controllable() as i32);
    i.extend(st.course_stars.map(|c| c as i32));
    debug_assert_eq!(i.len(), INT_COLS.len());

    let x = &mut f.flts;
    x.extend(st.pos);
    x.extend(st.vel);
    x.push(st.forward_vel);
    x.push(mem.f32(MARIO + MARIO_slideVelX));
    x.push(mem.f32(MARIO + MARIO_slideVelZ));
    x.push(mem.f32(MARIO + MARIO_peakHeight));
    x.push(mem.f32(MARIO + MARIO_quicksandDepth));
    x.push(mem.f32(MARIO + MARIO_intendedMag));
    x.push(st.floor_y);
    x.push(st.ceil_y);
    x.extend(st.floor_normal);
    x.extend(st.wall_normal.unwrap_or([0.0; 3]));
    x.push(st.floor_obj_vy.unwrap_or(0.0));
    x.push(st.floor_obj_y.unwrap_or(0.0));
    let mut boxes = geo::read_water(m);
    let hd = |b: &geo::EnvBox| {
        let cx = st.pos[0].clamp(b.lo[0], b.hi[0]) - st.pos[0];
        let cz = st.pos[2].clamp(b.lo[1], b.hi[1]) - st.pos[2];
        cx * cx + cz * cz
    };
    boxes.sort_by(|a, b| hd(a).total_cmp(&hd(b)));
    for k in 0..MAX_WATER {
        match boxes.get(k) {
            // id -1 marks an empty slot.
            Some(b) => x.extend([b.id as f32, b.lo[0], b.lo[1], b.hi[0], b.hi[1], b.y]),
            None => x.extend([-1.0, 0.0, 0.0, 0.0, 0.0, 0.0]),
        }
    }
    debug_assert_eq!(x.len(), FLT_COLS.len());

    // Objects: active, not Mario, not cosmetic; nearest MAX_OBJ.
    let mut objs: Vec<(f32, usize)> = (0..OBJECT_POOL_CAPACITY as usize)
        .filter_map(|s| {
            let o = POOL + s * OBJECT_SIZE;
            let active = mem.u16(o + OBJ_activeFlags) as u32;
            if active & ACTIVE_FLAG_ACTIVE == 0 || o == mario_obj {
                return None;
            }
            if crate::bhv_name(mem.u32(o + OBJ_behavior)).is_some_and(cosmetic) {
                return None;
            }
            let p = [mem.f32(o + OBJ_oPosX), mem.f32(o + OBJ_oPosY), mem.f32(o + OBJ_oPosZ)];
            Some((crate::dist(p, st.pos), s))
        })
        .collect();
    objs.sort_by(|a, b| a.0.total_cmp(&b.0));
    objs.truncate(MAX_OBJ);
    for &(_, s) in &objs {
        let o = POOL + s * OBJECT_SIZE;
        f.obj_ints.extend([
            s as i32,
            bhv_index(mem.u32(o + OBJ_behavior)),
            mem.i32(o + OBJ_oInteractType),
            mem.i32(o + OBJ_oInteractStatus),
            mem.i32(o + OBJ_oAction),
            mem.i32(o + OBJ_oSubAction),
            mem.i32(o + OBJ_oTimer),
            mem.i32(o + OBJ_oHealth),
            mem.i32(o + OBJ_oDamageOrCoinValue),
            mem.i32(o + OBJ_oNumLootCoins),
            mem.i32(o + OBJ_oBehParams),
            mem.i32(o + OBJ_oHeldState),
            mem.i32(o + OBJ_oIntangibleTimer),
            mem.i32(o + OBJ_oFaceAngleYaw),
            mem.i32(o + OBJ_oMoveAngleYaw),
            mem.i32(o + OBJ_oAngleVelYaw),
            mem.u16(o + OBJ_activeFlags) as i32,
            mem.i16(o + 0x1AC) as i32,
            mem.i16(o + 0x1AE) as i32,
            mem.i16(o + 0x1B0) as i32,
            mem.i16(o + 0x1B2) as i32,
            (st.held_slot == Some(s as u16)) as i32,
            (floor_owner == o) as i32,
        ]);
        f.obj_flts.extend([
            mem.f32(o + OBJ_oPosX),
            mem.f32(o + OBJ_oPosY),
            mem.f32(o + OBJ_oPosZ),
            mem.f32(o + OBJ_oHomeX),
            mem.f32(o + OBJ_oHomeY),
            mem.f32(o + OBJ_oHomeZ),
            mem.f32(o + OBJ_oVelX),
            mem.f32(o + OBJ_oVelY),
            mem.f32(o + OBJ_oVelZ),
            mem.f32(o + OBJ_oForwardVel),
            mem.f32(o + OBJ_hitboxRadius),
            mem.f32(o + OBJ_hitboxHeight),
            mem.f32(o + OBJ_hurtboxRadius),
            mem.f32(o + OBJ_hurtboxHeight),
        ]);
    }
    f.n_obj = objs.len();

    // Object-owned collision near Mario, from the dynamic partition.
    let mut tris = partition_tris(m, ADDR_gDynamicSurfacePartition as usize);
    let cd = |t: &[f32; 18]| {
        let c = [(t[0] + t[3] + t[6]) / 3.0, (t[1] + t[4] + t[7]) / 3.0, (t[2] + t[5] + t[8]) / 3.0];
        crate::dist(c, st.pos)
    };
    tris.sort_by(|a, b| cd(a).total_cmp(&cd(b)));
    tris.truncate(MAX_DSURF);
    f.n_tri = tris.len();
    for t in &tris {
        f.tris.extend_from_slice(t);
    }
    f
}

/// Every triangle of a spatial partition (static or dynamic), deduplicated,
/// as [`TRI_COLS`] rows.
pub fn partition_tris(m: &[u8], partition: usize) -> Vec<[f32; 18]> {
    let mem = Mem(m);
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    let cells = (NUM_CELLS * NUM_CELLS) as usize;
    for cell in 0..cells {
        for list in [SPATIAL_PARTITION_FLOORS, SPATIAL_PARTITION_CEILS, SPATIAL_PARTITION_WALLS] {
            let head = partition + cell * SPATIAL_PARTITION_CELL_SIZE + list as usize * SURFACE_NODE_SIZE;
            let mut node = mem.u32(head) as usize;
            let mut guard = 0;
            while node != 0 && node + 8 <= m.len() && guard < 100_000 {
                guard += 1;
                let s = mem.u32(node + 4) as usize;
                node = mem.u32(node) as usize;
                if s == 0 || s + SURFACE_SIZE > m.len() || !seen.insert(s) {
                    continue;
                }
                let v = |k: usize| mem.i16(s + 0x0A + k) as f32;
                let owner = mem.u32(s + SURFACE_object) as usize;
                let (slot, ov) = if owner >= POOL && owner < POOL + OBJECT_SIZE * OBJECT_POOL_CAPACITY as usize {
                    (((owner - POOL) / OBJECT_SIZE) as f32, [mem.f32(owner + OBJ_oVelX), mem.f32(owner + OBJ_oVelY), mem.f32(owner + OBJ_oVelZ)])
                } else {
                    (-1.0, [0.0; 3])
                };
                out.push([
                    v(0), v(2), v(4), v(6), v(8), v(10), v(12), v(14), v(16),
                    mem.f32(s + SURFACE_normal), mem.f32(s + SURFACE_normal + 4), mem.f32(s + SURFACE_normal + 8),
                    mem.i16(s + SURFACE_type) as f32, list as f32, slot, ov[0], ov[1], ov[2],
                ]);
            }
        }
    }
    out
}

/// The current area's static collision triangles, if the static partition
/// is surely this area's (Mario in control on a static floor in it; the
/// same test as [`geo::observe_floors`]).
pub fn zone_geometry(m: &[u8], st: &St) -> Option<Vec<[f32; 18]>> {
    if !st.controllable() || st.floor_obj.is_some() {
        return None;
    }
    let floor = Mem(m).u32(MARIO + MARIO_floor);
    if floor == 0 {
        return None;
    }
    let (_, seen) = geo::read_floors(m);
    if !seen.contains(&floor) {
        return None;
    }
    Some(partition_tris(m, ADDR_gStaticSurfacePartition as usize))
}

/// Per-frame event bits (sm64_AI_backbone.md 4, events head), from two
/// consecutive states (no objects needed).
pub const EVENT_NAMES: &[&str] = &[
    "death", "damage", "ledge_grab", "wall_kick_window", "blj_able", "enter_water", "warp", "coin",
    "star", "key", "red_coin", "grab", "dialog_open", "cannon", "lose_life", "land",
];

pub fn events(prev: &St, cur: &St) -> u32 {
    use crate::layout::*;
    const ACT_LEDGE_GRAB: u32 = 0x0800034B;
    const ACT_AIR_HIT_WALL: u32 = 0x000008A7;
    const ACT_LONG_JUMP: u32 = 0x03000888;
    let mut e = 0u32;
    let mut set = |k: usize, b: bool| e |= (b as u32) << k;
    set(0, prev.wedges() >= 1 && cur.wedges() < 1);
    set(1, cur.health < prev.health && cur.zone() == prev.zone());
    set(2, cur.action == ACT_LEDGE_GRAB && prev.action != ACT_LEDGE_GRAB);
    set(3, cur.action == ACT_AIR_HIT_WALL);
    set(4, cur.action == ACT_LONG_JUMP && cur.forward_vel < 0.0);
    set(5, cur.action & ACT_FLAG_SWIMMING != 0 && prev.action & ACT_FLAG_SWIMMING == 0);
    set(6, cur.zone() != prev.zone());
    set(7, cur.coins > prev.coins);
    set(8, cur.stars > prev.stars);
    set(9, cur.keys > prev.keys);
    set(10, cur.red_coins > prev.red_coins);
    set(11, cur.holding && !prev.holding);
    set(12, cur.dialog >= 0 && prev.dialog < 0);
    set(13, cur.action == ACT_IN_CANNON && prev.action != ACT_IN_CANNON);
    set(14, cur.lives < prev.lives);
    set(15, prev.action & ACT_FLAG_AIR != 0 && cur.action & ACT_FLAG_AIR == 0 && cur.action & ACT_FLAG_SWIMMING == 0);
    e
}

/// Static triangles a model input carries (nearest the state; GOAL_GEO of
/// them nearest the way to a goal when there is one), then up to MAX_DSURF
/// object triangles: GEO tokens (sm64_AI_backbone.md 2.5). Matches
/// backbone/sm64bb/data.py `Store.geometry`.
pub const N_STATIC: usize = 96;
pub const GOAL_GEO: usize = 24;
pub const N_GEO: usize = N_STATIC + MAX_DSURF;
pub const TRI_LEN: usize = 18;

/// One state as the model's input arrays (backbone `data.Store.state`):
/// padded to MAX_OBJ objects and N_GEO triangles, with masks.
#[derive(Debug, Clone)]
pub struct ModelInput {
    pub ints: Vec<i32>,
    pub flts: Vec<f32>,
    pub pad_prev: u32,
    pub obj_ints: Vec<i32>,
    pub obj_flts: Vec<f32>,
    pub obj_mask: Vec<bool>,
    pub tris: Vec<f32>,
    pub tri_mask: Vec<bool>,
}

fn centroid(t: &[f32; TRI_LEN]) -> [f32; 3] {
    [(t[0] + t[3] + t[6]) / 3.0, (t[1] + t[4] + t[7]) / 3.0, (t[2] + t[5] + t[8]) / 3.0]
}

/// Indices into `zone` of the static triangles to carry: GOAL_GEO nearest
/// the segment from `pos` toward `goal` (capped at 3000 units, slightly
/// favouring the near end) when the goal is more than 50 units away, then
/// the nearest to `pos` up to N_STATIC in all.
pub fn select_static(zone: &[[f32; TRI_LEN]], pos: [f32; 3], goal: Option<[f32; 3]>) -> Vec<usize> {
    let cent: Vec<[f32; 3]> = zone.iter().map(centroid).collect();
    let mut taken = vec![false; zone.len()];
    let mut out = Vec::with_capacity(N_STATIC);
    if let Some(g) = goal {
        let seg = [g[0] - pos[0], g[1] - pos[1], g[2] - pos[2]];
        let len = crate::dist(g, pos);
        if len > 50.0 {
            let u = seg.map(|v| v / len);
            let cap = len.min(3000.0);
            let mut ds: Vec<(f32, usize)> = cent
                .iter()
                .enumerate()
                .map(|(i, c)| {
                    let d = [c[0] - pos[0], c[1] - pos[1], c[2] - pos[2]];
                    let t = (d[0] * u[0] + d[1] * u[1] + d[2] * u[2]).clamp(0.0, cap);
                    let r = [d[0] - t * u[0], d[1] - t * u[1], d[2] - t * u[2]];
                    ((r[0] * r[0] + r[1] * r[1] + r[2] * r[2]).sqrt() + 0.1 * t, i)
                })
                .collect();
            ds.sort_by(|a, b| a.0.total_cmp(&b.0));
            for &(_, i) in ds.iter().take(GOAL_GEO) {
                taken[i] = true;
                out.push(i);
            }
        }
    }
    let mut near: Vec<(f32, usize)> =
        cent.iter().enumerate().filter(|(i, _)| !taken[*i]).map(|(i, c)| (crate::dist(*c, pos), i)).collect();
    near.sort_by(|a, b| a.0.total_cmp(&b.0));
    out.extend(near.iter().take(N_STATIC - out.len()).map(|&(_, i)| i));
    out
}

/// The model input for features `f` of a state at `pos`, with its zone's
/// static triangles (`None` while the zone's partition hasn't been read),
/// an optional goal position, and the pad applied on the frame that led to
/// the state (packed `button << 16 | x << 8 | y`).
pub fn model_input(f: &Features, zone: Option<&[[f32; TRI_LEN]]>, pos: [f32; 3], goal: Option<[f32; 3]>, pad_prev: u32) -> ModelInput {
    let (noi, nof) = (OBJ_INT_COLS.len(), OBJ_FLT_COLS.len());
    let mut obj_ints = vec![0; MAX_OBJ * noi];
    let mut obj_flts = vec![0.0; MAX_OBJ * nof];
    obj_ints[..f.obj_ints.len()].copy_from_slice(&f.obj_ints);
    obj_flts[..f.obj_flts.len()].copy_from_slice(&f.obj_flts);
    let mut obj_mask = vec![false; MAX_OBJ];
    obj_mask[..f.n_obj].fill(true);
    let mut tris = vec![0.0; N_GEO * TRI_LEN];
    let mut tri_mask = vec![false; N_GEO];
    if let Some(z) = zone {
        for (k, i) in select_static(z, pos, goal).into_iter().enumerate() {
            tris[k * TRI_LEN..(k + 1) * TRI_LEN].copy_from_slice(&z[i]);
            tri_mask[k] = true;
        }
    }
    tris[N_STATIC * TRI_LEN..N_STATIC * TRI_LEN + f.tris.len()].copy_from_slice(&f.tris);
    tri_mask[N_STATIC..N_STATIC + f.n_tri].fill(true);
    ModelInput { ints: f.ints.clone(), flts: f.flts.clone(), pad_prev, obj_ints, obj_flts, obj_mask, tris, tri_mask }
}

/// A pad packed as in the dataset and the model input (`button << 16 | x << 8 | y`).
pub fn pack_pad(p: crate::GamePad) -> u32 {
    (p.button as u32) << 16 | (p.stick_x as u8 as u32) << 8 | p.stick_y as u8 as u32
}

pub fn unpack_pad(v: u32) -> crate::GamePad {
    crate::GamePad::new((v >> 16) as u16, (v >> 8) as u8 as i8, v as u8 as i8)
}

// ---------------------------------------------------------------------------
// A process-wide cache of zones' static triangles, for building model inputs
// during a search (search/src/model.rs). Filled from `zone_geometry` the
// first time Mario is in control on a zone's static floor, or preloaded from
// the backbone dataset's zone tables (`<dir>/zones/<level>_<area>.f32`) so
// inputs match the training data exactly (as backbone/sm64bb/refs.py
// `make_pool` does).

type ZoneTris = std::sync::Arc<Vec<[f32; TRI_LEN]>>;

fn zone_cache() -> &'static std::sync::RwLock<std::collections::HashMap<(u16, u8), ZoneTris>> {
    static C: std::sync::OnceLock<std::sync::RwLock<std::collections::HashMap<(u16, u8), ZoneTris>>> = std::sync::OnceLock::new();
    C.get_or_init(Default::default)
}

/// Loads every `zones/<level>_<area>.f32` under each of `dirs`, keeping the
/// largest table per zone. Returns the number of zones known after.
pub fn preload_zones(dirs: &[&str]) -> usize {
    let mut c = zone_cache().write().unwrap();
    for d in dirs {
        let Ok(rd) = std::fs::read_dir(format!("{d}/zones")) else { continue };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            let Some(stem) = name.strip_suffix(".f32") else { continue };
            let Some((lv, ar)) = stem.split_once('_') else { continue };
            let (Ok(lv), Ok(ar)) = (lv.parse::<u16>(), ar.parse::<u8>()) else { continue };
            let Ok(bytes) = std::fs::read(e.path()) else { continue };
            let rows: Vec<[f32; TRI_LEN]> = bytes
                .chunks_exact(4 * TRI_LEN)
                .map(|r| std::array::from_fn(|k| f32::from_le_bytes(r[4 * k..4 * k + 4].try_into().unwrap())))
                .collect();
            if c.get(&(lv, ar)).is_none_or(|t| t.len() < rows.len()) {
                c.insert((lv, ar), std::sync::Arc::new(rows));
            }
        }
    }
    c.len()
}

/// The model input of the live game state in memory `m` (`st` read from it
/// with objects), using the shared zone cache.
pub fn live_input(m: &[u8], st: &St, goal: Option<[f32; 3]>, pad_prev: u32) -> ModelInput {
    let key = st.zone();
    let mut zone = zone_cache().read().unwrap().get(&key).cloned();
    if zone.is_none() {
        if let Some(t) = zone_geometry(m, st) {
            zone = Some(zone_cache().write().unwrap().entry(key).or_insert_with(|| std::sync::Arc::new(t)).clone());
        }
    }
    let f = read_features(m, st);
    model_input(&f, zone.as_deref().map(|v| v.as_slice()), st.pos, goal, pad_prev)
}

/// A model input as bytes in the backbone server's order (backbone/serve_model.py):
/// gi i32, gf f32, pad_prev u32, oi i32, of f32, om u8, tri f32, tm u8.
pub fn input_bytes(x: &ModelInput) -> Vec<u8> {
    let mut b = Vec::with_capacity(20_000);
    b.extend(x.ints.iter().flat_map(|v| v.to_le_bytes()));
    b.extend(x.flts.iter().flat_map(|v| v.to_le_bytes()));
    b.extend(x.pad_prev.to_le_bytes());
    b.extend(x.obj_ints.iter().flat_map(|v| v.to_le_bytes()));
    b.extend(x.obj_flts.iter().flat_map(|v| v.to_le_bytes()));
    b.extend(x.obj_mask.iter().map(|&v| v as u8));
    b.extend(x.tris.iter().flat_map(|v| v.to_le_bytes()));
    b.extend(x.tri_mask.iter().map(|&v| v as u8));
    b
}
