"""Replays an .m64 as the wanted input and shows how the perturbation breaks it.

  python examples/replay_bot.py [route.m64] [seed]
"""
import sys

from sm64coin import GamePad, load_m64, load_rom, practice

path = sys.argv[1] if len(sys.argv) > 1 else "pow/miner/routes/block1-human.m64"
seed = int(sys.argv[2]) if len(sys.argv) > 2 else 1
route = load_m64(path)
poked = []


def bot(env, stop):
    poked.clear()
    while not env.is_over() and not stop():
        f = env.frame
        wanted = route[f] if f < len(route) else GamePad()
        if env.step(wanted) != wanted:
            poked.append(f)


load_rom()
r = practice(bot, seed=seed)
print(f"{path}: {len(route)} frames; {len(poked)} poked frames, first at {poked[:1]}; ran {r['frames']} frames, star={r['success']}")
