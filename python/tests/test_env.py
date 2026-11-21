"""Python proofs must verify natively. Needs the ROM and the Rust example
`verify_file` (cargo build --release -p sm64coin-sdk --examples)."""
import json
import os
import random
import subprocess

import pytest

from sm64coin import GamePad, MiningEnv, Seed, Task, buttons, load_rom, params, seal
from sm64coin.env import pad_bytes
from sm64coin.perturb import Perturber, PokeConfig

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
VERIFY = os.path.join(ROOT, "pow", "target", "release", "examples", "verify_file")
FIXTURE = os.path.join(ROOT, "pow", "sdk", "fixtures", "win_seed1_d1.json")
ROM = os.path.join(ROOT, "baserom.us.z64")


@pytest.fixture(scope="session", autouse=True)
def rom():
    load_rom(os.environ.get("SM64COIN_ROM", ROM))


def verify(task, env, tmp_path, *flags, blob=False):
    """Native verification: of the pads played on every frame, or (blob=True) of the sealed proof."""
    f = tmp_path / "pads.bin"
    f.write_bytes(env.proof() if blob else env.proof_raw())
    if blob:
        flags = (*flags, "--blob")
    r = subprocess.run([VERIFY, task.seed.hex(), str(task.difficulty), str(f), "--rom", os.environ.get("SM64COIN_ROM", ROM), *flags],
                       capture_output=True, text=True)
    return r.returncode, r.stdout.strip()


def pad_at(rng, f):
    """Nothing for the intro (random presses keep the game on the title screen, whose clock
    stands still: nothing can be poked), then random."""
    if f < 1100:
        return GamePad()
    return GamePad(buttons.A_BUTTON if rng.random() < .3 else 0, rng.randint(-80, 80), rng.randint(-80, 80))


def test_seed_word_and_params():
    s = Seed(bytes(range(1, 33)))
    assert len({s.word(k) for k in range(8)}) == 8 and s.word(0) != s.word(8)
    assert Seed(bytes(32)).word(0) == 1
    assert (params(1).max_frames, params(1).random_amount) == (18000, 5)


def test_params_match_rust():
    for d in (1, 2, 3, 7, 16, 64, 1000, 10**6, 10**12):
        out = subprocess.run([VERIFY, "--params", str(d)], capture_output=True, text=True).stdout.split()
        p = params(d)
        assert [int(x) for x in out] == [p.max_frames, p.random_amount, p.window, p.burst, p.reseed_frames], d


def test_random_run_conforms(tmp_path):
    for d in (1, 64):
        task = Task.practice(11, d)
        env = MiningEnv(task)
        rng = random.Random(3)
        poked = 0
        for f in range(2000):
            w = pad_at(rng, f)
            poked += env.step(w) != w
        assert poked > 50
        code, out = verify(task, env, tmp_path, "--conformance")
        assert code == 0, out


def test_ignoring_the_perturbation_is_caught(tmp_path):
    task = Task.practice(5, 1)
    game = load_rom().create_game()
    rng = random.Random(9)
    pads = []
    for f in range(2000):
        p = pad_at(rng, f)
        game.step_game(p)  # no perturbation
        pads.append(p)
    f = tmp_path / "pads.bin"
    f.write_bytes(b"".join(pad_bytes(p) for p in pads))
    r = subprocess.run([VERIFY, task.seed.hex(), "1", str(f), "--rom", os.environ.get("SM64COIN_ROM", ROM), "--conformance"],
                       capture_output=True, text=True)
    assert r.returncode == 1 and "NONCONFORMING" in r.stdout


def test_perturbation_budget_and_clock():
    """Pure-Python checks of the generator (no game): budget per window, no pokes/draws on a standing clock."""
    cfg = PokeConfig(100, 5, 2)
    p = Perturber(bytes([9] * 32), cfg)
    s0 = list(p.s)
    for _ in range(300):
        assert p.pad(42, GamePad()) == (GamePad(), False)
    assert p.s == s0
    p = Perturber(bytes([9] * 32), cfg)
    poked = 0
    for t in range(1000):
        poked += p.pad(t, GamePad())[1]
        p.mix([t])
    assert 90 <= poked <= 110, poked


def test_seal_proof_stores_only_unpoked_inputs(tmp_path):
    task = Task.practice(7, 1)
    env = MiningEnv(task)
    rng = random.Random(4)
    for f in range(2000):
        env.step(pad_at(rng, f))
    n_poked = len(env.pads) - len(env.inputs)
    assert n_poked > 20
    b = env.proof()
    assert b[:2] == bytes([2000 % 128 | 0x80, 2000 // 128]) and len(b) == 2 + 1 + 4 * len(env.inputs)
    # The sealed blob decodes natively and its replay agrees with the pads played (it just doesn't win).
    code, out = verify(task, env, tmp_path, blob=True)
    assert code == 1 and "success condition never met" in out, out


def test_fork_save_restore():
    env = MiningEnv(Task.practice(2, 1))
    rng = random.Random(1)
    for f in range(1000):
        env.step(pad_at(rng, f))
    snap, fork = env.save(), env.fork()
    fut = [pad_at(rng, 1100) for _ in range(200)]
    for w in fut:
        env.step(w)
        fork.step(w)
    assert env.pads == fork.pads and env.state() == fork.state()
    env.restore(snap)
    assert env.frame == 1000
    for w in fut:
        env.step(w)
    assert env.pads == fork.pads and env.state() == fork.state()


@pytest.mark.skip(reason="fixture win_seed1_d1.json predates the external perturbation (old in-game pokes); regenerate with make_fixture")
def test_winning_fixture_verifies_natively(tmp_path):
    j = json.load(open(FIXTURE))
    task = Task(Seed.from_hex(j["seed_hash"]), j["difficulty"])
    raw = bytes.fromhex(j["pads_raw_hex"])
    env = MiningEnv(task)
    for i in range(0, len(raw), 4):
        b = int.from_bytes(raw[i:i + 2], "little")
        x, y = raw[i + 2], raw[i + 3]
        assert not env.success
        w = GamePad(b, x - 256 if x > 127 else x, y - 256 if y > 127 else y)
        assert env.step(w) == w
    assert env.success and len(env.pads) == len(raw) // 4 > 600
    assert env.proof_raw() == raw
    with pytest.raises(RuntimeError):
        env.step(GamePad())
    assert len(env.inputs) < len(env.pads), "pokes aren't stored"
    code, out = verify(task, env, tmp_path)
    assert code == 0 and out.startswith("VALID"), out
    code, out = verify(task, env, tmp_path, blob=True)  # the sealed proof (non-poked inputs only)
    assert code == 0 and out.startswith("VALID"), out


def test_seal_roundtrip():
    blob = bytes([1, 2, 3, 0]).hex()
    assert seal(blob, b"x" * 130) == (bytes([1, 2, 3, 0x82, 0x01]) + b"x" * 130).hex()
