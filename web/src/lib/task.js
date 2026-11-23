import { GameConfig } from 'sm64-binds-frontend';

// A GameConfig-like object for a task as the daemon reports it (get_info,
// get_block_template and get_block all carry these sm64_* fields). The
// perturbation's parameters (`random_config`) are SM64Coin's own: the game
// bindings no longer know about them (sm64-binds 2.1), lib/perturb.js reads them.
export function gameConfig(t) {
  const c = new GameConfig(t.sm64_max_frames);
  c.random_config = {
    window_length: t.sm64_window_length,
    random_amount: t.sm64_random_amount,
    random_burst_length: t.sm64_burst_length,
    A_prob: t.sm64_a_prob,
    B_prob: t.sm64_b_prob,
    Z_prob: t.sm64_z_prob,
    // Frames between state mixes into the poke timing / pad streams (0: never).
    timing_mix: t.sm64_timing_mix ?? 1,
    pad_mix: t.sm64_pad_mix ?? 1,
  };
  return c;
}

/** A daemon task result with its GameConfig attached, for recordRun/playRun. */
export const withConfig = (t) => ({ ...t, config: gameConfig(t) });
