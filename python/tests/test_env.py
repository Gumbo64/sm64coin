"""Python proofs must verify natively. Needs the ROM and the Rust example
`verify_file` (cargo build --release -p sm64coin-sdk --examples)."""
import json
import os
import random
import subprocess

import pytest

from sm64coin import GamePad, MiningEnv, Seed, Task, buttons, load_rom, params, seal

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
VERIFY = os.path.join(ROOT, "pow", "target", "release", "examples", "verify_file")
FIXTURE = os.path.join(ROOT, "pow", "sdk", "fixtures", "win_seed1_d1.json")
ROM = os.path.join(ROOT, "baserom.us.z64")


@pytest.fixture(scope="session", autouse=True)
def rom():
    load_rom(os.environ.get("SM64COIN_ROM", ROM))


def verify(task, env, tmp_path, *flags):
    f = tmp_path / "pads.bin"
    f.write_bytes(env.proof_raw())
    r = subprocess.run([VERIFY, task.seed.hex(), str(task.difficulty), str(f), "--rom", os.environ.get("SM64COIN_ROM", ROM), *flags],
                       capture_output=True, text=True)
    return r.returncode, r.stdout.strip()


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


def test_random_run_conforms_across_reseeds(tmp_path):
    for d in (1, 64):
        task = Task.practice(11, d)
        env = MiningEnv(task)
        rng = random.Random(3)
        poked = 0
        for _ in range(1000):  # crosses reseeds at 300, 600, 900
            w = GamePad(buttons.A_BUTTON if rng.random() < .3 else 0, rng.randint(-80, 80), rng.randint(-80, 80))
            poked += env.step(w) != w
        assert poked > 50
        code, out = verify(task, env, tmp_path, "--conformance")
        assert code == 0, out


def test_forgetting_reseed_is_caught(tmp_path):
    task = Task.practice(5, 1)
    env = MiningEnv(task)
    env.task.params = task.params.__class__(**{**task.params.__dict__, "reseed_frames": 10**9})  # never reseed
    rng = random.Random(9)
    for _ in range(700):
        env.step(GamePad(0, rng.randint(-80, 80), rng.randint(-80, 80)))
    code, out = verify(task, env, tmp_path, "--conformance")
    assert code == 1 and "NONCONFORMING" in out


def test_fork_save_restore():
    env = MiningEnv(Task.practice(2, 1))
    rng = random.Random(1)
    mk = lambda: GamePad(buttons.A_BUTTON if rng.random() < .3 else 0, rng.randint(-80, 80), rng.randint(-80, 80))
    for _ in range(290):
        env.step(mk())
    snap, fork = env.save(), env.fork()
    fut = [mk() for _ in range(40)]
    for w in fut:
        env.step(w)
        fork.step(w)
    assert env.pads == fork.pads and env.state() == fork.state()
    env.restore(snap)
    assert env.frame == 290
    for w in fut:
        env.step(w)
    assert env.pads == fork.pads and env.state() == fork.state()


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
    code, out = verify(task, env, tmp_path)
    assert code == 0 and out.startswith("VALID"), out


def test_seal_roundtrip():
    blob = bytes([1, 2, 3, 0]).hex()
    assert seal(blob, b"x" * 130) == (bytes([1, 2, 3, 0x82, 0x01]) + b"x" * 130).hex()
