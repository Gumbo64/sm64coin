"""A fresh payout subaddress per template (v0.4, docs/V0.4-DESIGN.md section 3.2, decision 13).

A proof record links its run to the payout address its template commits (`sm64_payout_address`), so a miner
that holds its wallet's secret view key commits the subaddress (0, k) for each template, k from a counter kept
next to the signed file. The block reward itself still pays the main address (`wallet_address`).
Mirror of pow/miner/src/payout.rs and `sm64pow::payout::subaddress` (Monero's derivation:
`m = H_s("SubAddr\\0" || a || major LE32 || minor LE32)`, `D = B + m G`, `C = a D`)."""

import os

from .keccak import keccak256
from .sign import _L, _P, _D, _add, _base, _hash_to_scalar

_ALPHABET = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"
_SIZES = [0, 2, 3, 5, 6, 7, 9, 10, 11]

# standard address prefix -> subaddress prefix (cryptonote_config.h: mainnet, testnet, stagenet)
SUBADDRESS_PREFIX = {705687: 2245783, 53: 63, 24: 36}


def b58_encode(data: bytes) -> str:
    out = []
    for i in range(0, len(data), 8):
        blk = data[i:i + 8]
        n = int.from_bytes(blk, "big")
        s = [b"1"[0]] * _SIZES[len(blk)]
        for j in range(len(s) - 1, -1, -1):
            s[j] = _ALPHABET[n % 58]
            n //= 58
        out.append(bytes(s).decode())
    return "".join(out)


def b58_decode(text: str) -> bytes:
    raw, out = text.encode(), bytearray()
    for i in range(0, len(raw), 11):
        blk = raw[i:i + 11]
        if len(blk) not in _SIZES:
            raise ValueError("bad base58 length")
        size = _SIZES.index(len(blk))
        n = 0
        for c in blk:
            d = _ALPHABET.find(bytes([c]))
            if d < 0:
                raise ValueError("bad base58 character")
            n = n * 58 + d
        if n >= 1 << (8 * size):
            raise ValueError("base58 block overflows")
        out += n.to_bytes(size, "big")
    return bytes(out)


def _varint(v: int) -> bytes:
    out = bytearray()
    while v >= 0x80:
        out.append(v & 0x7F | 0x80)
        v >>= 7
    out.append(v)
    return bytes(out)


def decode_address(addr: str):
    """(prefix, spend public, view public) of a standard address or subaddress; raises ValueError."""
    b = b58_decode(addr)
    if len(b) < 69:
        raise ValueError("address too short")
    body, check = b[:-4], b[-4:]
    if keccak256(body)[:4] != check:
        raise ValueError("address checksum mismatch")
    prefix, shift, i = 0, 0, 0
    while True:
        if i >= len(body) or shift > 63:
            raise ValueError("bad prefix")
        c = body[i]
        prefix |= (c & 0x7F) << shift
        i += 1
        shift += 7
        if c & 0x80 == 0:
            break
    if len(body) != i + 64:
        raise ValueError("bad address length")
    return prefix, body[i:i + 32], body[i + 32:]


def encode_address(prefix: int, spend: bytes, view: bytes) -> str:
    d = _varint(prefix) + spend + view
    return b58_encode(d + keccak256(d)[:4])


# ---- ed25519 point arithmetic beyond scalar-mult-base ----

def _decompress(b: bytes):
    y = int.from_bytes(b, "little")
    sign, y = y >> 255, y & ((1 << 255) - 1)
    if y >= _P:
        raise ValueError("not a point")
    x2 = (y * y - 1) * pow(_D * y * y + 1, -1, _P) % _P
    x = pow(x2, (_P + 3) // 8, _P)
    if (x * x - x2) % _P:
        x = x * pow(2, (_P - 1) // 4, _P) % _P
    if (x * x - x2) % _P:
        raise ValueError("not a point")
    if x & 1 != sign:
        x = _P - x
    return (x, y, 1, x * y % _P)


def _compress(p) -> bytes:
    zi = pow(p[2], -1, _P)
    x, y = p[0] * zi % _P, p[1] * zi % _P
    return (y | (x & 1) << 255).to_bytes(32, "little")


def _mul(p, s: int):
    r, q = (0, 1, 1, 0), p
    while s:
        if s & 1:
            r = _add(r, q)
        q = _add(q, q)
        s >>= 1
    return r


def subaddress_keys(view_secret: bytes, spend_public: bytes, major: int, minor: int):
    """(D, C): the spend and view public keys of subaddress (major, minor); (0, 0) is the main address."""
    a = int.from_bytes(view_secret, "little")
    if len(view_secret) != 32 or a >= _L or a == 0:
        raise ValueError("the view secret is not a canonical scalar")
    spend = _decompress(spend_public)
    if (major, minor) == (0, 0):
        return spend_public, _compress(_mul(_base(), a))
    m = _hash_to_scalar(b"SubAddr\0" + view_secret + major.to_bytes(4, "little") + minor.to_bytes(4, "little"))
    d = _add(spend, _mul(_base(), m))
    return _compress(d), _compress(_mul(d, a))


class Payouts:
    """Hands out subaddresses (0, k) of one wallet. `main_address` is the wallet's main address, `view_secret`
    its secret view key (hex; checked against the address). The counter lives in `<signed_file>.payout`
    (kept in memory when `signed_file` is empty): k starts at 1 ((0, 0) is the main address) and is persisted
    before it is used, so a restart never reuses one."""

    def __init__(self, main_address: str, view_secret: str, signed_file: str = ""):
        self.main = main_address
        self.prefix, self.spend, view = decode_address(main_address)
        if self.prefix not in SUBADDRESS_PREFIX:
            raise ValueError(f"the address is not a standard SM64Coin address (unknown prefix {self.prefix})")
        self.view_secret = bytes.fromhex(view_secret.strip())
        if subaddress_keys(self.view_secret, self.spend, 0, 0)[1] != view:
            raise ValueError("the view secret does not match the address")
        self.counter_file = f"{signed_file}.payout" if signed_file else None
        self.k = 0

    def address(self, minor: int) -> str:
        d, c = subaddress_keys(self.view_secret, self.spend, 0, minor)
        return encode_address(SUBADDRESS_PREFIX[self.prefix], d, c)

    def next(self) -> str:
        k = self.k
        if self.counter_file:
            try:
                with open(self.counter_file) as f:
                    k = int(f.read().strip())
            except (OSError, ValueError):
                k = self.k
        k += 1
        self.k = k
        if self.counter_file:
            d = os.path.dirname(self.counter_file)
            if d:
                os.makedirs(d, exist_ok=True)
            with open(self.counter_file, "w") as f:
                f.write(str(k))
        return self.address(k)
