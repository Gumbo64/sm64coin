"""One run per key (v0.3): the signed-seed record and the lead before a run is valid."""
import time

from sm64coin.miner import SignedStore, wait_until_valid


def test_one_run_per_seed_survives_a_restart(tmp_path):
    f = str(tmp_path / "signed.json")
    a, b = bytes([3, 0, 0, 0, 0, 1, 2]), bytes([3, 0, 0, 0, 0, 9, 9])
    s = SignedStore(f)
    assert s.signed_frames("seed", a) is None
    s.record("seed", a, 100)
    assert s.signed_frames("seed", a) is None, "the same run again is the same block"
    assert s.signed_frames("seed", b) == 100, "a second, different run is equivocation"
    assert s.signed_frames("other", b) is None
    assert SignedStore(f).signed_frames("seed", b) == 100, "remembered after a restart"
    assert SignedStore("").signed_frames("seed", b) is None, "no file: memory only"


class _Daemon:
    def __init__(self, at):
        self.at = at

    def valid_at(self, address, key, frames):
        return self.at


class _T:
    max_frames = 10
    parent_timestamp = 0
    key = None
    retarget_difficulty = 1
    difficulty = 1


def test_wait_returns_lead_seconds_before_the_stamp():
    at = int(time.time()) + 3
    t0 = time.time()
    ts = wait_until_valid(_Daemon(at), "SM6", _T(), 99, lambda: False, log=lambda *_: None, poll=0.1, lead=2)
    assert ts == at, "stamped with the second the run becomes valid"
    assert 0.5 <= time.time() - t0 <= 2.5, "returned about `lead` seconds early, not at `at`"


def test_wait_gives_up_when_the_tip_moves():
    at = int(time.time()) + 30
    assert wait_until_valid(_Daemon(at), "SM6", _T(), 99, lambda: True, log=lambda *_: None, lead=5) is None
