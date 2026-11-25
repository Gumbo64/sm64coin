"""Signed proofs (v0.3 keys; v0.4 committed proof_id): the miner key and the block signature, in pure Python.

Every block must be signed by a throwaway per-template key. The miner generates a `Keypair`,
asks the daemon for a template with `extra_nonce=key.extra_nonce()` (the daemon puts it in the
coinbase, so the seed covers the key), plays the run, then signs it: `sign_proof(key, seed, proof)`.
The scheme is Monero's own Schnorr signature on ed25519 with Keccak (`crypto::generate_signature`);
mirror of pow/sm64pow/src/sign.rs, checked byte for byte against Rust (tests/sig_vectors_web_v04.jsonl).
"""

import os

from .keccak import keccak256, keccak512, _keccak  # noqa: F401  (re-exported)
from .proof import MAX_FRAMES, decode_proof, get_varint, pad_bytes, proof_id, split  # noqa: F401

KEY_TAG = 0x64
MSG_TAG = b"sm64coin proof signature v1"
NONCE_TAG = b"sm64coin signing nonce v1"

# ---- ed25519: just enough for scalar-mult-base ----

_P = 2**255 - 19
_L = 2**252 + 27742317777372353535851937790883648493
_D = -121665 * pow(121666, -1, _P) % _P
_BY = 4 * pow(5, -1, _P) % _P


def _base():
    y = _BY
    x2 = (y * y - 1) * pow(_D * y * y + 1, -1, _P) % _P
    x = pow(x2, (_P + 3) // 8, _P)
    if (x * x - x2) % _P:
        x = x * pow(2, (_P - 1) // 4, _P) % _P
    if x & 1:
        x = _P - x
    return (x, y, 1, x * y % _P)


def _add(p, q):
    """Extended twisted-Edwards addition (a = -1)."""
    a = (p[1] - p[0]) * (q[1] - q[0]) % _P
    b = (p[1] + p[0]) * (q[1] + q[0]) % _P
    c = 2 * _D * p[3] * q[3] % _P
    d = 2 * p[2] * q[2] % _P
    e, f, g, h = b - a, d - c, d + c, b + a
    return (e * f % _P, g * h % _P, f * g % _P, e * h % _P)


def _mul_base(s: int) -> bytes:
    """Compressed s*G."""
    r, q = (0, 1, 1, 0), _base()
    while s:
        if s & 1:
            r = _add(r, q)
        q = _add(q, q)
        s >>= 1
    zi = pow(r[2], -1, _P)
    x, y = r[0] * zi % _P, r[1] * zi % _P
    return (y | (x & 1) << 255).to_bytes(32, "little")


def _scalar(b: bytes) -> int:
    return int.from_bytes(b, "little") % _L


def _hash_to_scalar(data: bytes) -> int:
    return _scalar(keccak256(data))


# ---- keys and signatures ----

class Keypair:
    """A miner key: `secret` (canonical scalar, 32 bytes) and `public` (compressed point)."""

    def __init__(self, secret: bytes):
        """From a canonical secret scalar, e.g. the key a group leader shares."""
        if len(secret) != 32 or int.from_bytes(secret, "little") >= _L or not any(secret):
            raise ValueError("secret must be a non-zero canonical scalar (32 bytes, little endian, below l)")
        self.secret = bytes(secret)
        self.public = _mul_base(int.from_bytes(secret, "little"))

    @classmethod
    def from_entropy(cls, entropy: bytes) -> "Keypair":
        """From 32 random bytes (reduced mod l, like Rust's `Keypair::from_entropy`)."""
        return cls((_scalar(entropy)).to_bytes(32, "little"))

    @classmethod
    def generate(cls) -> "Keypair":
        while True:
            try:
                return cls.from_entropy(os.urandom(32))
            except ValueError:
                pass

    def extra_nonce(self) -> bytes:
        """What to request the template with: `0x64 || public` (33 bytes)."""
        return bytes([KEY_TAG]) + self.public

    def sign(self, msg: bytes) -> bytes:
        """Monero's Schnorr signature `c || r` over a 32-byte message (deterministic)."""
        x = int.from_bytes(self.secret, "little")
        ctr = 0
        while True:
            k = int.from_bytes(keccak512(NONCE_TAG + self.secret + msg + bytes([ctr])), "little") % _L
            c = _hash_to_scalar(msg + self.public + _mul_base(k))
            r = (k - c * x) % _L
            if c and r and k:
                return c.to_bytes(32, "little") + r.to_bytes(32, "little")
            ctr = (ctr + 1) & 0xFF

    def hex_secret(self) -> str:
        return self.secret.hex()


def proof_id_v03(blob: bytes) -> bytes:
    """v0.3's proof_id (no committed header): Keccak-256 of `"sm64run2" || frames u32 LE || start || 4 bytes
    per input pad` of a v0.3-layout blob, codecs 0-2. Kept for the v0.3 vectors (tests/sig_vectors_web_v04.jsonl)."""
    from .proof import decode_proof_v03
    p = decode_proof_v03(blob, MAX_FRAMES)
    return keccak256(b"sm64run2" + p.frames.to_bytes(4, "little") + bytes([p.start]) + b"".join(pad_bytes(g) for g in p.inputs))


def message(seed: bytes, pid: bytes, frames: int) -> bytes:
    """What the signature signs: the template's seed, the run's proof_id and its length in frames."""
    return keccak256(MSG_TAG + seed + pid + frames.to_bytes(4, "little"))


def sign_proof(key: Keypair, seed: bytes, proof: bytes) -> bytes:
    """The block's `sm64_sig` (64 bytes) for a sealed proof blob. `seed` is the template's
    `sm64_seed_hash` (bytes, or its hex string), requested with `extra_nonce=key.extra_nonce()`."""
    if isinstance(seed, str):
        seed = bytes.fromhex(seed)
    frames = split(proof)[0]    # the committed header must parse: the id is the hash it carries
    return key.sign(message(seed, proof_id(proof), frames))
