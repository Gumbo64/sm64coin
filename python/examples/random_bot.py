"""Random inputs: will essentially never win; shows the API (a block is the next point of progress:
a star, a Bowser key, a cap switch or the Grand Star, from power-on in practice, from the parent end state on the chain).

  python examples/random_bot.py                              # offline practice
  python examples/random_bot.py http://127.0.0.1:16481 SM64...   # mine
"""
import random
import sys

from sm64coin import GamePad, buttons, practice, run_miner, load_rom


def bot(env, stop):
    rng = random.Random(1)
    while not env.is_over() and not stop():
        wanted = GamePad(buttons.A_BUTTON if rng.random() < 0.3 else 0, rng.randint(-80, 80), rng.randint(-80, 80))
        played = env.step(wanted)  # differs from `wanted` on poked frames


if __name__ == "__main__":
    if len(sys.argv) >= 3:
        run_miner(bot, sys.argv[1], sys.argv[2])
    else:
        load_rom()
        r = practice(bot, seed=1, difficulty=1)
        print(f"{r['frames']}/{r['max_frames']} frames, success={r['success']}, {r['fps']:.0f} frames/s")
