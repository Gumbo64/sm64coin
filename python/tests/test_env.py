"""Python vs Rust agreement and pure-Python checks of the 70-star cycle SDK.

Needs the ROM (SM64COIN_ROM or ../baserom.us.z64). Tests that compare with the Rust
implementation need `verify_file` (cargo build --release -p sm64coin-sdk --examples) and are
skipped when it is not built. The winning-run test replays the Rust fixture
pow/sm64pow/tests/fixtures/power_on_star.json."""
import dataclasses
import json
import os
import random
import subprocess

import pytest

from sm64coin import (event, GamePad, MiningEnv, Progress, Seed, Task, buttons, decode_proof, encode_proof, load_rom,
                      max_frames, params, replay_pads, seal, verify)
from sm64coin import rules
from sm64coin.env import pad_bytes
from sm64coin.perturb import (COURSE_STARS, ACT_JUMBO_STAR_CUTSCENE, HAVE_KEY_2, MARIO_ACTION, MARIO_NUM_STARS, MARIO_SIZE,
                              SAVE_FILE_SIZE, SAVE_FLAGS, UNLOCKED_BASEMENT_DOOR, Perturber, PokeConfig, progress,
                              progress_regions_len)
from sm64coin.proof import varint

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
VERIFY = os.path.join(ROOT, "pow", "target", "release", "examples", "verify_file")
FIXTURE = os.path.join(ROOT, "pow", "sm64pow", "tests", "fixtures", "power_on_star.json")
ROM = os.path.join(ROOT, "baserom.us.z64")

RULES_RS = os.path.join(ROOT, "pow", "sm64pow", "src", "rules.rs")
need_verify = pytest.mark.skipif(
    not os.path.exists(VERIFY) or os.path.getmtime(VERIFY) < os.path.getmtime(RULES_RS),
    reason="Rust example verify_file not built, or older than the 70-star-cycle rules.rs "
           "(cargo build --release -p sm64coin-sdk --examples)")


@pytest.fixture(scope="session", autouse=True)
def rom():
    load_rom(os.environ.get("SM64COIN_ROM", ROM))


def verify_native(task, env, tmp_path, *flags, blob=False):
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


def test_seed_word():
    s = Seed(bytes(range(1, 33)))
    assert len({s.word(k) for k in range(8)}) == 8 and s.word(0) != s.word(8)
    assert Seed(bytes(32)).word(0) == 1


def test_params_and_limits_match_rust_tests():
    """Expectations of the Rust tests in rules.rs (`StarCycle`)."""
    assert (rules.level8(0), rules.level8(1), rules.level8(2), rules.level8(3), rules.level8(4), rules.level8(256)) == (0, 0, 8, 12, 16, 64)
    assert rules.level8(2**64 - 1) == 63 * 8 + 7
    last = params(1)
    assert (last.max_frames, last.random_amount) == (18000, 5)
    for d in range(2, 5000):
        p = params(d)
        assert p.max_frames <= last.max_frames
        assert (p.random_amount, p.window, p.burst) == (5, 100, 2), "pokes don't depend on difficulty"
        last = p
    assert params(2**64 - 1).max_frames == 600
    # effective difficulty (decay_halves_after_grace)
    e = rules.effective_difficulty
    assert (e(64, 0), e(64, 600), e(64, 900), e(64, 1200), e(64, 1_000_000), e(0, 0)) == (64, 64, 32, 16, 1, 1)
    # time_limit_grows_every_second_after_the_grace
    d = 1 << 10
    assert max_frames(d, 0) == params(d).max_frames and max_frames(d, 600) == max_frames(d, 0)
    last, grew = max_frames(d, 600), 0
    for s in range(601, 600 + 300 * 10):
        m = max_frames(d, s)
        assert m >= last
        grew += m > last
        last = m
    assert grew > 2000
    assert max_frames(d, 900) == params(d // 2).max_frames
    assert max_frames(d, 10_000_000) == params(1).max_frames
    assert max_frames(2**64 - 1, 0) == rules.MIN_FRAMES == 600
    assert max_frames(2**64 - 1, 2**64 - 1) == params(1).max_frames or max_frames(2**64 - 1, 2**64 - 1) >= 600
    # valid_after inverts it
    for frames in (5000, 12000, 18000):
        s = rules.valid_after(d, frames)
        assert max_frames(d, s) >= frames and (s == 0 or max_frames(d, s - 1) < frames)
    assert rules.valid_after(d, 18001) is None


def test_success_and_cycle_end():
    s = Progress(12, 1, False)
    assert not rules.is_success(s, s)
    assert rules.is_success(s, Progress(13, 1, False)) and rules.is_success(s, Progress(12, 2, False))
    assert not rules.ends_cycle(s) and rules.ends_cycle(Progress(70, 2, True))
    assert (rules.stall_seconds(), rules.target_seconds()) == (3600, 300)


def test_progress_counts_stars_keys_and_the_grand_star():
    """Mirror of the Rust unit test of the same name (pow/perturb/src/sm64.rs)."""
    n = progress_regions_len()
    assert n == MARIO_SIZE + SAVE_FILE_SIZE * 2 * 4 + 2
    b = bytearray(n)
    assert progress(bytes(b)).points() == 0
    b[MARIO_NUM_STARS] = 3
    b[n - 2] = 2  # file 2 (1-based)
    flags = MARIO_SIZE + 2 * SAVE_FILE_SIZE + SAVE_FLAGS  # file 2, first copy
    b[flags] = UNLOCKED_BASEMENT_DOOR | HAVE_KEY_2
    b[MARIO_SIZE + 2 * SAVE_FILE_SIZE + COURSE_STARS + 8] = 0b101  # course 9: stars 1 and 3
    p = progress(bytes(b))
    assert (p.stars, p.keys, p.grand_star) == (3, 2, False)
    assert p.star_flags[9] == 0b101 and sum(p.star_flags) == 0b101
    q = dataclasses.replace(p, star_flags=tuple(f | 0b10 if i == 9 else f for i, f in enumerate(p.star_flags)))
    assert event(p, q) == "c09s2" and event(p, p) is None
    assert event(p, dataclasses.replace(p, keys=3)) == "key3"
    assert event(p, dataclasses.replace(p, grand_star=True)) == "grand"
    r = dataclasses.replace(p, star_flags=(0b1000,) + p.star_flags[1:])
    assert event(p, r) == "c00s4"
    b[MARIO_ACTION:MARIO_ACTION + 4] = ACT_JUMBO_STAR_CUTSCENE.to_bytes(4, "little")
    assert not progress(bytes(b)).grand_star, "needs 70 stars"
    b[MARIO_NUM_STARS] = 70
    assert progress(bytes(b)).points() == 73
    b[n - 2] = 9  # no such file: no keys
    assert progress(bytes(b)).keys == 0


def test_proof_encoding_round_trip():
    pads = [GamePad(0x8000, -80, 80), GamePad(0, 0, 0), GamePad(0x1234, 3, -4)]
    for start in (0, 1):
        blob = encode_proof(300, start, pads)
        assert blob == varint(300) + bytes([start, 0]) + b"".join(pad_bytes(p) for p in pads)
        assert blob[:2] == bytes([300 % 128 | 0x80, 300 // 128])
        pr = decode_proof(blob, 18000)
        assert (pr.frames, pr.start, pr.inputs) == (300, start, pads)
    with pytest.raises(ValueError):
        decode_proof(encode_proof(300, 0, pads), 299)       # over the limit
    with pytest.raises(ValueError):
        decode_proof(varint(300) + b"\x02\x00", 400)        # unknown start
    with pytest.raises(ValueError):
        decode_proof(encode_proof(2, 0, pads), 400)         # more inputs than frames
    with pytest.raises(ValueError):
        decode_proof(b"\x80\x00\x00\x00", 400)             # non-canonical varint
    # the run-length codec (id 1): runs of identical pads
    p = GamePad(0, 0, 0)
    rle = varint(60) + b"\x00" + b"\x01" + varint(50) + pad_bytes(p) + varint(10) + pad_bytes(GamePad(0x8000, 1, 2))
    assert decode_proof(rle, 100).inputs == [p] * 50 + [GamePad(0x8000, 1, 2)] * 10


def test_seal_rewrites_the_timestamp_and_appends_the_proof():
    # header: major 1, minor 1, timestamp 1_000_000_000 (5 bytes), prev_id (32), nonce (4), rest, empty proof 0
    hdr = bytes([1, 1]) + varint(1_000_000_000) + bytes(32) + bytes(4) + b"tail"
    blob = (hdr + b"\x00").hex()
    out = bytes.fromhex(seal(blob, b"x" * 130, 5))
    assert out == bytes([1, 1, 5]) + bytes(32) + bytes(4) + b"tail" + bytes([0x82, 0x01]) + b"x" * 130
    out = bytes.fromhex(seal(blob, b"", 2_000_000_000))
    assert out == bytes([1, 1]) + varint(2_000_000_000) + bytes(32) + bytes(4) + b"tail" + b"\x00"
    import time
    ts, i = 0, 2
    out = bytes.fromhex(seal(blob, b"ab"))  # default: now
    shift = 0
    while True:
        byte = out[i]; i += 1
        ts |= (byte & 0x7F) << shift; shift += 7
        if not byte & 0x80:
            break
    assert abs(ts - time.time()) < 5
    with pytest.raises(ValueError):
        seal((hdr + b"\x01").hex(), b"")


@need_verify
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
        code, out = verify_native(task, env, tmp_path, "--conformance")
        assert code == 0, out


@need_verify
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


def test_seal_proof_stores_only_unpoked_inputs():
    task = Task.practice(7, 1)
    env = MiningEnv(task)
    rng = random.Random(4)
    for f in range(2000):
        env.step(pad_at(rng, f))
    n_poked = len(env.pads) - len(env.inputs)
    assert n_poked > 20
    b = env.proof()
    assert b[:2] == bytes([2000 % 128 | 0x80, 2000 // 128]) and b[2:4] == b"\x01\x00"  # power-on start, raw codec
    assert len(b) == 2 + 1 + 1 + 4 * len(env.inputs)
    # No star: the replay agrees with the pads played but finds no progress.
    ok, why = verify(task, b, max_frames=2000)
    assert not ok and "no progress" in why, why


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




def test_winning_fixture_verifies():
    """The Rust fixture (a real power-on run to the first star, xz codec): Python's replay agrees."""
    j = json.load(open(FIXTURE))
    task = Task(Seed.from_hex(j["seed_hash"]), 1)
    blob = bytes.fromhex(j["proof_hex"])
    assert task.start == "power-on" and task.max_frames == 18000
    pr = decode_proof(blob, 18000)
    assert (pr.frames, pr.start) == (j["frames"], 1)
    ok, info = verify(task, blob)
    assert ok and info == j["frames"], info
    # Driving a MiningEnv with the decoded inputs wins on exactly the last frame; the poked frames
    # (regenerated from the seed, never stored) consume no input.
    env = MiningEnv(task)
    while not env.success:
        n = len(env.inputs)
        env.step(pr.inputs[n] if n < len(pr.inputs) else GamePad())
    assert env.frame == j["frames"] and (env.progress.stars, env.progress.keys, env.progress.grand_star) == (1, 0, False) and sum(map(bool, env.progress.star_flags)) == 1
    assert env.inputs == pr.inputs and len(pr.inputs) < pr.frames
    # our own raw-codec proof of the same run is valid too
    assert env.proof() != blob and verify(task, env.proof()) == (True, j["frames"])
    with pytest.raises(RuntimeError):
        env.step(GamePad())
    ok, why = verify(task, blob, max_frames=j["frames"] - 1)
    assert not ok and "3850" in why
    parent = Task(Seed.from_hex(j["seed_hash"]), 1, start="parent", start_game=load_rom().create_game())
    ok, why = verify(parent, blob)
    assert not ok and "starts from" in why
    ok, why = verify(Task(Seed(bytes(range(32))), 1), blob)  # another seed: other pokes
    assert not ok
    ok, why = verify(task, blob[:-9])  # truncated xz stream
    assert not ok


def test_parent_start_is_relative_to_the_parent_progress():
    task = Task.practice(3, 1, start_pads=[GamePad()] * 40)
    assert task.start == "parent" and task.start_progress == Progress(0, 0, False) and task.start_byte == 0
    env = MiningEnv(task)
    assert env.frame == 0 and not env.success
    for _ in range(30):
        env.step(GamePad())

    assert env.proof()[len(varint(30))] == 0, "parent start byte"
    # the task's start game is copied, never advanced by a run
    assert MiningEnv(task).game.get_game_state() == task.start_game.get_game_state()
    # a start that already has a point: the same state is no success
    t2 = Task(task.seed, 1, start="parent", start_game=task.start_game, start_progress=Progress(1, 0, False))
    assert t2.start_progress.points() == 1 and not MiningEnv(t2).success
    with pytest.raises(ValueError):
        Task(task.seed, 1, start="parent")


def test_replay_pads_is_plain_stepping():
    pads = [GamePad()] * 20
    a, b = replay_pads(pads), load_rom().create_game()
    for p in pads:
        b.step_game(p)
    assert a.read_memory(0xb0a010, 176) == b.read_memory(0xb0a010, 176)


def _template(**kw):
    from sm64coin import Template
    d = 1 << 10
    base = dict(blocktemplate_blob="00", difficulty=d, height=5, prev_hash="aa" * 32, expected_reward=1,
                seed_hash="11" * 32, reseed_frames=0, max_frames=max_frames(d, 700), random_amount=5,
                window_length=100, burst_length=2, timestamp=10_700, retarget_difficulty=d, start="parent",
                parent_timestamp=10_000)
    base.update(kw)
    return Template(**base)


def test_task_from_template_builds_the_start_from_the_cycle():
    from sm64coin import Cycle
    cyc = Cycle(pads=[GamePad()] * 25, stars=0, keys=0)
    t = _template()
    task = Task.from_template(t, cyc)
    assert task.start == "parent" and task.max_frames == max_frames(1 << 10, 700) > params(1 << 10).max_frames
    assert task.start_progress == Progress(0, 0, False) and task.play_frames == task.max_frames
    assert Task.from_template(t, cyc, play_frames=18000).play_frames == 18000
    with pytest.raises(ValueError):
        Task.from_template(t)  # a parent start needs the cycle
    # "either": power-on is possible too; a stalled chain may demand it
    assert Task.from_template(_template(start="either"), cyc, "power-on").start == "power-on"
    assert Task.from_template(_template(start="power-on")).start == "power-on"
    with pytest.raises(ValueError):
        Task.from_template(_template(start="power-on"), cyc, "parent")
    with pytest.raises(ValueError):
        Task.from_template(_template(start="parent"), cyc, "power-on")
    # a stale rules copy is refused: other time limit, other pokes, other rules
    for bad in (dict(max_frames=1234), dict(random_amount=9), dict(rules="any-star")):
        with pytest.raises(ValueError):
            Task.from_template(_template(**bad), cyc)
    # the daemon's idea of the parent's progress must match the replay
    with pytest.raises(RuntimeError):
        Task.from_template(t, Cycle(pads=[GamePad()] * 25, stars=3))
