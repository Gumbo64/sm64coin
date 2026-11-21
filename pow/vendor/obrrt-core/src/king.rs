//! King Bob-omb's throws (`OBRRT_KING_LANDING=1`, objectives/src/boss.rs
//! and spec's `thrown_off`): where a thrown King lands, on the summit or off
//! it. Pure functions; nothing here runs unless an objective asks.
//!
//! king_bobomb.inc.c: thrown (act 4, `cur_obj_get_thrown_or_placed`, then
//! `cur_obj_move_standard(-78)`: gravity 4 / frame from SET_OBJ_PHYSICS
//! -400 / 100, terminal speed 78), he loses health on landing only if
//! `oPosY - oHomeY > -100`; lower, he lands off the hill, plays an animation
//! and (act 5) jumps back home (oVelY 100, then `arc_to_goal_pos` with 100
//! up / -4 gravity), and when Mario comes within 500 he speaks again
//! (DIALOG_128) before the fight goes on (act 2).

/// The summit's floor triangles (x, z) at y 4190-4294: levels/bob/areas/1/
/// collision.inc.c, SURFACE_BOSS_FIGHT_CAMERA and the flat ones around it.
const ARENA: [[(f32, f32); 3]; 9] = [
    [(4402.0, -5375.0), (4735.0, -3647.0), (4846.0, -5631.0)],
    [(4402.0, -5375.0), (4513.0, -3775.0), (4735.0, -3647.0)],
    [(4846.0, -3583.0), (4846.0, -5631.0), (4735.0, -3647.0)],
    [(4402.0, -3839.0), (4513.0, -3775.0), (4402.0, -5375.0)],
    [(3072.0, -6655.0), (4402.0, -5375.0), (4846.0, -5631.0)],
    [(3072.0, -6655.0), (4402.0, -3839.0), (4402.0, -5375.0)],
    [(3072.0, -6655.0), (1298.0, -3583.0), (3072.0, -3071.0)],
    [(3072.0, -6655.0), (1298.0, -5631.0), (1298.0, -3583.0)],
    [(3072.0, -6655.0), (3072.0, -3071.0), (4402.0, -3839.0)],
];

/// The summit floor's height (the King's home after DROP_TO_FLOOR).
pub const ARENA_Y: f32 = 4293.0;
/// A point well inside the arena, to steer throws at.
pub const ARENA_CENTRE: [f32; 3] = [2600.0, 4293.0, -4800.0];
const GRAVITY: f32 = 4.0;
const TERMINAL: f32 = 78.0;
/// Below home by more than this when he lands: off the hill.
pub const OFF_DY: f32 = 100.0;

fn inside(x: f32, z: f32, t: &[(f32, f32); 3]) -> bool {
    let d = |a: (f32, f32), b: (f32, f32)| (b.0 - a.0) * (z - a.1) - (b.1 - a.1) * (x - a.0);
    let (s1, s2, s3) = (d(t[0], t[1]), d(t[1], t[2]), d(t[2], t[0]));
    (s1 >= 0.0 && s2 >= 0.0 && s3 >= 0.0) || (s1 <= 0.0 && s2 <= 0.0 && s3 <= 0.0)
}

/// Whether (x, z) is over the summit's floor.
pub fn on_arena(x: f32, z: f32) -> bool {
    ARENA.iter().any(|t| inside(x, z, t))
}

/// Whether a thrown King at `pos`, at `prev` one frame earlier, lands on
/// the summit (`home_y` his home height): his flight integrated at constant
/// horizontal speed and gravity 4 until he is at the floor over the arena
/// (on) or more than OFF_DY below home (off).
pub fn lands_on_stage(prev: [f32; 3], pos: [f32; 3], home_y: f32) -> bool {
    let (vx, vz) = (pos[0] - prev[0], pos[2] - prev[2]);
    let mut vy = pos[1] - prev[1];
    let (mut x, mut y, mut z) = (pos[0], pos[1], pos[2]);
    for _ in 0..300 {
        if y - home_y <= -OFF_DY {
            return false;
        }
        if vy <= 0.0 && y <= home_y + 1.0 && on_arena(x, z) {
            return true;
        }
        vy = (vy - GRAVITY).max(-TERMINAL);
        x += vx;
        z += vz;
        y += vy;
    }
    on_arena(x, z)
}

/// Whether the King is off the hill now: thrown and already below the
/// line, or jumping back (act 5).
pub fn is_off(action: i32, pos_y: f32, home_y: f32) -> bool {
    action == 5 || (action == 4 && pos_y - home_y <= -OFF_DY)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// From his home (by the west edge, x 1298), a throw west lands off the
    /// hill and one east on it; a fall from below the line is off.
    #[test]
    fn throws() {
        let home = [1636.0, ARENA_Y, -5567.0];
        let at = [home[0], home[1] + 70.0, home[2]];
        let west = [at[0] + 25.0, at[1] - 40.0, at[2]];
        let east = [at[0] - 25.0, at[1] - 40.0, at[2]];
        assert!(!lands_on_stage(west, at, home[1]));
        assert!(lands_on_stage(east, at, home[1]));
        assert!(on_arena(ARENA_CENTRE[0], ARENA_CENTRE[2]));
        assert!(is_off(4, home[1] - 150.0, home[1]) && is_off(5, home[1], home[1]) && !is_off(4, home[1], home[1]));
    }
}
