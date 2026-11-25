"""Signed proofs (v0.3): the miner key and the block signature, in pure Python.

Every block must be signed by a throwaway per-template key. The miner generates a `Keypair`,
asks the daemon for a template with `extra_nonce=key.extra_nonce()` (the daemon puts it in the
coinbase, so the seed covers the key), plays the run, then signs it: `sign_proof(key, seed, proof)`.
The scheme is Monero's own Schnorr signature on ed25519 with Keccak (`crypto::generate_signature`);
mirror of pow/sm64pow/src/sign.rs, checked byte for byte against Rust (tests/sig_vectors.jsonl).
"""

import os

from .proof import MAX_FRAMES, decode_proof, get_varint, pad_bytes

KEY_TAG = 0x64
MSG_TAG = b"sm64coin proof signature v1"
NONCE_TAG = b"sm64coin signing nonce v1"

# ---- Keccak (the original padding, as Monero's cn_fast_hash; hashlib.sha3_* pad differently) ----

_RC = [
    0x0000000000000001, 0x0000000000008082, 0x800000000000808A, 0x8000000080008000, 0x000000000000808B,
    0x0000000080000001, 0x8000000080008081, 0x8000000000008009, 0x000000000000008A, 0x0000000000000088,
    0x0000000080008009, 0x000000008000000A, 0x000000008000808B, 0x800000000000008B, 0x8000000000008089,
    0x8000000000008003, 0x8000000000008002, 0x8000000000000080, 0x000000000000800A, 0x800000008000000A,
    0x8000000080008081, 0x8000000000008080, 0x0000000080000001, 0x8000000080008008]
_ROT = [[0, 36, 3, 41, 18], [1, 44, 10, 45, 2], [62, 6, 43, 15, 61], [28, 55, 25, 21, 56], [27, 20, 39, 8, 14]]
_M64 = (1 << 64) - 1


def _rol(x, n):
    return ((x << n) | (x >> (64 - n))) & _M64 if n else x


def _permute(a):
    """Keccak-f[1600] on a[x][y]."""
    for rc in _RC:
        c = [a[x][0] ^ a[x][1] ^ a[x][2] ^ a[x][3] ^ a[x][4] for x in range(5)]
        d = [c[(x - 1) % 5] ^ _rol(c[(x + 1) % 5], 1) for x in range(5)]
        a = [[a[x][y] ^ d[x] for y in range(5)] for x in range(5)]
        b = [[0] * 5 for _ in range(5)]
        for x in range(5):
            for y in range(5):
                b[y][(2 * x + 3 * y) % 5] = _rol(a[x][y], _ROT[x][y])
        a = [[b[x][y] ^ (~b[(x + 1) % 5][y] & b[(x + 2) % 5][y]) for y in range(5)] for x in range(5)]
        a[0][0] ^= rc
    return a


def _keccak(data: bytes, rate: int, out_len: int) -> bytes:
    msg = bytearray(data)
    msg.append(0x01)                              # original Keccak padding (SHA-3 uses 0x06)
    msg.extend(b"\0" * (-len(msg) % rate))
    msg[-1] |= 0x80
    a = [[0] * 5 for _ in range(5)]
    for off in range(0, len(msg), rate):
        for i in range(rate // 8):
            a[i % 5][i // 5] ^= int.from_bytes(msg[off + 8 * i:off + 8 * i + 8], "little")
        a = _permute(a)
    out = b"".join(a[i % 5][i // 5].to_bytes(8, "little") for i in range(25))
    return out[:out_len]


def keccak256(data: bytes) -> bytes:
    """Monero's `cn_fast_hash`."""
    return _keccak(data, 136, 32)


def keccak512(data: bytes) -> bytes:
    return _keccak(data, 72, 64)


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


def proof_id(blob: bytes) -> bytes:
    """What a block id commits to for its proof (pow/sm64pow/src/proof_id.rs), codecs 0-2: Keccak-256
    of `"sm64run2" || frames u32 LE || start || 4 bytes per input pad`, so re-encoding a proof with
    another codec does not change it. State codecs (3-5) hash their bytes after a replay: use the Rust SDK."""
    p = decode_proof(blob, MAX_FRAMES)    # raises on state codecs and malformed blobs
    return keccak256(b"sm64run2" + p.frames.to_bytes(4, "little") + bytes([p.start]) + b"".join(pad_bytes(g) for g in p.inputs))


def message(seed: bytes, pid: bytes, frames: int) -> bytes:
    """What the signature signs: the template's seed, the run's proof_id and its length in frames."""
    return keccak256(MSG_TAG + seed + pid + frames.to_bytes(4, "little"))


def sign_proof(key: Keypair, seed: bytes, proof: bytes) -> bytes:
    """The block's `sm64_sig` (64 bytes) for a sealed proof blob. `seed` is the template's
    `sm64_seed_hash` (bytes, or its hex string), requested with `extra_nonce=key.extra_nonce()`."""
    if isinstance(seed, str):
        seed = bytes.fromhex(seed)
    frames, _ = get_varint(proof, 0)
    return key.sign(message(seed, proof_id(proof), frames))
