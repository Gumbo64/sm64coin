"""Signed proofs: keccak vectors, and Rust cross-checks: tests/sig_vectors.jsonl (fixed data,
raw / run-length / xz proofs, made with an earlier version of the Rust example) and
tests/sig_vectors_web.jsonl (`cargo run --release -p sm64pow --example sig_vectors`, the vectors the
browser miner is checked against)."""

import json
import pathlib

import pytest

from sm64coin.daemon import seal
from sm64coin.proof import varint
from sm64coin.proof import decode_proof, commit, input_hash, proof_id, split
from sm64coin.sign import Keypair, keccak256, keccak512, message, proof_id_v03, sign_proof

VECTORS = [json.loads(l) for l in (pathlib.Path(__file__).parent / "sig_vectors.jsonl").read_text().splitlines() if l]


def test_keccak_known_vectors():
    assert keccak256(b"").hex() == "c5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470"
    assert keccak256(b"abc").hex() == "4e03657aea45a94fc7d47ba826c8d667c0d1e6e33a64a036ec44f58fa12d6c45"
    assert keccak512(b"").hex().startswith("0eab42de4c3ceb9235fc91acffe746b29c29a8c366b7c60e4e67c466f36a4304")
    # across the rate boundary (136 bytes)
    assert len(keccak256(b"x" * 136)) == 32 and keccak256(b"x" * 135) != keccak256(b"x" * 136)


@pytest.mark.parametrize("v", VECTORS, ids=[v["name"] for v in VECTORS])
def test_matches_rust(v):
    key = Keypair.from_entropy(bytes.fromhex(v["entropy"]))
    assert key.secret.hex() == v["secret"]
    assert key.public.hex() == v["public"]
    assert key.extra_nonce().hex() == "64" + v["public"]
    blob = bytes.fromhex(v["proof_hex"])
    assert proof_id_v03(blob).hex() == v["proof_id"]
    # v0.3 signature: the same message over the v0.3 id
    frames = v["frames"]
    assert key.sign(message(bytes.fromhex(v["seed"]), proof_id_v03(blob), frames)).hex() == v["sig"]


WEB = [json.loads(l) for l in (pathlib.Path(__file__).parent / "sig_vectors_web_v04.jsonl").read_text().splitlines() if l]


@pytest.mark.parametrize("v", WEB)
def test_matches_rust_web_vectors(v):
    key = Keypair.from_entropy(bytes.fromhex(v["entropy"]))
    assert key.public.hex() == v["public"]
    assert key.extra_nonce().hex() == v["extra_nonce"]
    blob = bytes.fromhex(v["proof"])
    assert proof_id(blob).hex() == v["proof_id"]
    assert split(blob)[0] == v["frames"]
    assert sign_proof(key, bytes.fromhex(v["seed"]), blob).hex() == v["sig"]


V04 = [json.loads(l) for l in (pathlib.Path(__file__).parent.parent.parent / "web/wallet-wasm/test/vectors-v04.jsonl").read_text().splitlines() if l]


@pytest.mark.parametrize("v", V04, ids=[v["name"] for v in V04])
def test_v04_vectors(v):
    blob = bytes.fromhex(v["blob"])
    assert proof_id(blob).hex() == v["id"]
    try:
        frames, start, h, enc = split(blob)
        parsed = True
    except ValueError:
        parsed = False
    assert parsed == v["header"]
    if parsed:
        assert (frames, start) == (v["frames"], v["start"])
        assert h.hex() == v["id"]
    try:
        decode_proof(blob)
        ok = True
    except ValueError:
        ok = False
    assert ok == (v["stateless"])
    if v.get("kind") == "run":
        pads = bytes.fromhex(v["pads"])
        assert input_hash(v["frames"], v["start"], pads).hex() == v["input_hash"]
        if v["name"].endswith("/raw"):
            assert commit(v["frames"], v["start"], pads, 0) == blob
        codec = blob[len(blob) - len(enc)] if enc else 0
        if codec in (0, 1):
            assert commit(v["frames"], v["start"], pads, codec) == blob
        else:
            assert proof_id(commit(v["frames"], v["start"], pads, 2)) == proof_id(blob)


def test_proof_id_is_codec_independent():
    by_frames = {}
    for v in VECTORS:
        by_frames.setdefault(v["frames"], set()).add(v["proof_id"])
    assert all(len(s) == 1 for s in by_frames.values())


def test_keypair_from_secret_and_generate():
    k = Keypair.generate()
    assert Keypair(k.secret).public == k.public
    with pytest.raises(ValueError):
        Keypair(bytes(32))
    with pytest.raises(ValueError):
        Keypair(b"\xff" * 32)


def test_seal_with_signature():
    ts_old, ts_new = 1000, 123456
    head = varint(16) + varint(16) + varint(ts_old) + bytes(32) + bytes(4) + b"tail"
    template = (head + b"\0" + bytes(64) + b"\0").hex()   # v0.4: empty proof, signature, proof records
    proof, sig = b"\x01\x00\x00\xaa\xbb\xcc\xdd", bytes(range(64))
    out = bytes.fromhex(seal(template, proof, sig, ts_new))
    assert out == varint(16) + varint(16) + varint(ts_new) + bytes(32) + bytes(4) + b"tail" + varint(len(proof)) + proof + sig + b"\0"
    with pytest.raises(ValueError):
        seal(head.hex() + "00", proof, sig, 1)   # old-style unsigned template
    with pytest.raises(ValueError):
        seal((head + b"\0" + bytes(64)).hex(), proof, sig, 1)   # a v0.3 template
