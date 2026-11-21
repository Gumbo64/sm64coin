import { GameConfig, RngConfig } from 'sm64-binds-frontend';

// A GameConfig for a task as the daemon reports it (get_info,
// get_block_template and get_block all carry these sm64_* fields).
export function gameConfig(t) {
  return new GameConfig(
    t.sm64_max_frames,
    new RngConfig(t.sm64_window_length, t.sm64_random_amount, t.sm64_burst_length, t.sm64_a_prob, t.sm64_b_prob, t.sm64_z_prob),
  );
}

/** A daemon task result with its GameConfig attached, for recordRun/playRun. */
export const withConfig = (t) => ({ ...t, config: gameConfig(t) });
