"""v0.4 miner behaviour (docs/V0.4-DESIGN.md): records-ready wait, payout subaddress per template, proof for a lost run."""
import pytest

from sm64coin import rules
from sm64coin.daemon import Daemon, wait_records_ready
from sm64coin.miner import SignedStore, publish_proof
from sm64coin.payout import Payouts, b58_decode, b58_encode, decode_address, encode_address, subaddress_keys

# A testnet wallet (pow testnet scripts) and the subaddress (0, 1) the Rust miner committed in a real template
# (`automine --payout-view-secret`): the TLV's spend and view keys.
SPEND = "6a59e3b08b5973897f3b73a45def0a5ed4b4f0179341d3acfdddb9509c2c2deb"
VIEW_SECRET = "f1ddb8505da58bfe13f6491089f18cd2287e8e29a1834b28e853fb994874de01"
VIEW_PUBLIC = "da996d9eeb377c1a80f583818c1824696d7ee6f6d95998ec0f51f8684476cbe6"
MAIN = "9wCQSFe1CAEPztoYE5sgtHGrykHJ2yNDxVwEgNwFpAvpgT5YRnvQTZD5S7w8PoEpEoJdnJ2rWpao1gV5RK41UegiT4AAkjG"
SUB1 = "BfjERdMn9eKRrHnUoqjoQydqDzfawTwjEAd89sXejvoDMiPRyp4MQnLMBfRY4TU7uxEzsVHz1GU8xVJAy9YEfmozMoNf4Qy"


def test_base58_and_address_codec():
    for n in range(40):
        d = bytes((i * 37 + 11) & 255 for i in range(n))
        assert b58_decode(b58_encode(d)) == d
    donate = "44AFFq5kSiGBoZ4NMDwYtN18obc8AemS33DBLWs3H7otXft3XjrpDtQGv7SqSsaBYBb98uNbr2VBBEt7f2wfn3RVGQBEP3A"
    p, s, v = decode_address(donate)
    assert p == 18 and encode_address(p, s, v) == donate
    with pytest.raises(ValueError):
        decode_address(donate.replace("44AFF", "44AFG"))


def test_main_address_and_subaddress_match_rust():
    prefix, spend, view = decode_address(MAIN)
    assert prefix == 53 and spend.hex() == SPEND and view.hex() == VIEW_PUBLIC
    assert subaddress_keys(bytes.fromhex(VIEW_SECRET), spend, 0, 0)[1].hex() == VIEW_PUBLIC
    p = Payouts(MAIN, VIEW_SECRET)
    assert p.address(1) == SUB1, "byte for byte the subaddress the Rust miner put in its template"
    assert decode_address(SUB1)[0] == 63 and SUB1.startswith("B")


def test_payouts_counter_persists_and_checks_the_secret(tmp_path):
    f = str(tmp_path / "signed.json")
    with pytest.raises(ValueError):
        Payouts(MAIN, "08" + "00" * 31, f)
    p = Payouts(MAIN, VIEW_SECRET, f)
    a, b = p.next(), p.next()
    assert a == SUB1 and a != b and b == p.address(2)
    assert Payouts(MAIN, VIEW_SECRET, f).next() == p.address(3), "a restart never reuses one"


def test_wait_for_records_ready():
    clock = [1000.0]
    slept = []

    def sleep(s):
        slept.append(s)
        clock[0] += s

    asks = []

    def fetch():
        asks.append(clock[0])
        return {"sm64_records_ready_at": 0}

    r = wait_records_ready(fetch, {"sm64_records_ready_at": 1040}, now=lambda: clock[0], sleep=sleep)
    assert sum(slept) == 40 and asks == [1040.0] and r == {"sm64_records_ready_at": 0}
    # already ready (or absent): no wait, no second request
    assert wait_records_ready(fetch, {"sm64_records_ready_at": 900}, now=lambda: clock[0], sleep=sleep) == {"sm64_records_ready_at": 900}
    assert len(asks) == 1
    # a node whose clock is far off: bounded
    clock[0] = 0
    wait_records_ready(lambda: {}, {"sm64_records_ready_at": 10 ** 9}, max_wait=120, now=lambda: clock[0], sleep=sleep)
    assert clock[0] == 120
    # a new block (stop) ends the wait
    r = wait_records_ready(fetch, {"sm64_records_ready_at": clock[0] + 99, "x": 1}, stop=lambda: True, now=lambda: clock[0], sleep=sleep)
    assert r["x"] == 1


class _Rpc(Daemon):
    def __init__(self, answers):
        super().__init__("http://x")
        self.answers, self.calls = list(answers), []

    def call(self, method, params=None):
        self.calls.append((method, params))
        if method == "get_info":
            return {"top_block_hash": "aa"}
        return self.answers.pop(0) if self.answers[0:1] and method == "get_block_template" else {}


def _tpl(ready):
    return {"blocktemplate_blob": "00", "difficulty": 1, "height": 5, "prev_hash": "pp", "expected_reward": 1,
            "sm64_seed_hash": "ss", "sm64_reseed_frames": 1, "sm64_max_frames": 9, "sm64_random_amount": 1,
            "sm64_records_ready_at": ready}


def test_get_template_sends_payout_and_waits(monkeypatch):
    import sm64coin.daemon as dm
    from sm64coin.sign import Keypair
    now = [5000.0]
    monkeypatch.setattr(dm.time, "time", lambda: now[0])
    monkeypatch.setattr(dm.time, "sleep", lambda s: now.__setitem__(0, now[0] + s))
    d = _Rpc([_tpl(5030), _tpl(0)])
    t = d.get_template(MAIN, Keypair.generate(), payout=SUB1, wait_ready=True)
    gbt = [p for m, p in d.calls if m == "get_block_template"]
    assert len(gbt) == 2 and all(p["sm64_payout_address"] == SUB1 and p["wallet_address"] == MAIN for p in gbt)
    assert t.records_ready_at == 0 and t.payout_address == SUB1 and now[0] >= 5030
    d2 = _Rpc([_tpl(0)])
    d2.get_template(MAIN, Keypair.generate())
    assert "sm64_payout_address" not in [p for m, p in d2.calls if m == "get_block_template"][0]


def test_archive_window_matches_rust_vector():
    # docs/V0.4-DESIGN.md: window_frames(1, 3950, 60) = 4,650
    assert rules.window_frames(1, 3950, 60) == 4650
    assert rules.window_frames(1, 3950, 0) >= 3950


class _T:
    height, seed_hash, difficulty, retarget_difficulty = 7, "seed", 1, 1
    key = None


class _D:
    def __init__(self, winner, verdict=(1, "")):
        self.winner, self.verdict, self.sealed, self.sent = winner, verdict, [], []

    def winner_frames(self, h):
        assert h == 7
        return self.winner

    def seal(self, t, proof, ts=None):
        self.sealed.append((proof, ts))
        return "hex"

    def submit_proof(self, blob):
        self.sent.append(blob)
        return self.verdict


def test_lost_run_is_published_within_the_window_and_signed_once():
    log = []
    proof, other = bytes([3, 0, 0, 0, 0, 1, 2]), bytes([3, 0, 0, 0, 0, 9, 9])
    s = SignedStore("")
    d = _D(3950)
    assert publish_proof(d, s, _T, proof, 4131, log.append) == 1 and d.sent == ["hex"]
    assert s.signed_frames("seed", other) == 4131, "recorded: one run per key"
    assert publish_proof(d, s, _T, other, 4000, log.append) is None and len(d.sent) == 1, "never a second run for the seed"
    assert publish_proof(d, s, _T, proof, 4131, log.append) == 1, "the same run again is the same record"
    assert "published as a proof for height 7" in log[0]


def test_lost_run_outside_the_window_or_unknown_winner():
    log = []
    d = _D(3950)
    assert publish_proof(d, SignedStore(""), _T, b"\x03abc", 4782, log.append) is None and not d.sent
    assert "outside the archive window" in log[0]
    d = _D(None, (0, "known"))  # winner unknown: the node decides
    assert publish_proof(d, SignedStore(""), _T, b"\x03abc", 4782, log.append) == 0 and d.sent
    assert "ignored" in log[-1]
