#!/usr/bin/env python3
"""Эталонная реализация примитивов MTProto на Python и сверка с ядром.

Написана независимо от C++ — по тексту спецификации:
  core.telegram.org/mtproto/description   (конверт MTProto 2.0)
  core.telegram.org/mtproto/auth_key      (RSA_PAD, pq, DH, tmp_aes_key, new_nonce_hash)
  core.telegram.org/api/srp               (SRP для 2FA)
  core.telegram.org/mtproto/mtproto-transports (obfuscated2)
Криптографические примитивы — пакет `cryptography` (AES-ECB/CTR) и hashlib.

Где у протокола есть вторая сторона (DH, SRP, обфускация), эталон играет и её:
проверяется не только совпадение с ядром, но и то, что сервер получает тот же
ключ / то же M.

Использование: reference.py <путь к refcheck> [число случаев на примитив]
"""
from __future__ import annotations

import hashlib
import os
import random
import subprocess
import sys

try:
    from cryptography.hazmat.primitives.ciphers import Cipher, algorithms, modes
except ImportError:  # понятная ошибка вместо traceback
    sys.exit("нужен пакет python cryptography (pacman -S python-cryptography)")

sha1 = lambda b: hashlib.sha1(b).digest()
sha256 = lambda b: hashlib.sha256(b).digest()
rnd = os.urandom

# dh_prime из core.telegram.org/mtproto/auth_key, п. 6 (скопирован из текста страницы).
DH_PRIME_HEX = (
    "C7 1C AE B9 C6 B1 C9 04 8E 6C 52 2F 70 F1 3F 73 98 0D 40 23 8E 3E 21 C1 49 34 D0 37 56 3D 93 0F 48 19 8A 0A "
    "A7 C1 40 58 22 94 93 D2 25 30 F4 DB FA 33 6F 6E 0A C9 25 13 95 43 AE D4 4C CE 7C 37 20 FD 51 F6 94 58 70 5A "
    "C6 8C D4 FE 6B 6B 13 AB DC 97 46 51 29 69 32 84 54 F1 8F AF 8C 59 5F 64 24 77 FE 96 BB 2A 94 1D 5B CD 1D 4A "
    "C8 CC 49 88 07 08 FA 9B 37 8E 3C 4F 3A 90 60 BE E6 7C F9 A4 A4 A6 95 81 10 51 90 7E 16 27 53 B5 6B 0F 6B 41 "
    "0D BA 74 D8 A8 4B 2A 14 B3 14 4E 0E F1 28 47 54 FD 17 ED 95 0D 59 65 B4 B9 DD 46 58 2D B1 17 8D 16 9C 6B C4 "
    "65 B0 D6 FF 9C A3 92 8F EF 5B 9A E4 E4 18 FC 15 E8 3E BE A0 F8 7F A9 FF 5E ED 70 05 0D ED 28 49 F4 7B F9 59 "
    "D9 56 85 0C E9 29 85 1F 0D 81 15 F6 35 B1 05 EE 2E 4E 15 D0 4B 24 54 BF 6F 4F AD F0 34 B1 04 03 11 9C D8 E3 "
    "B9 2F CC 5B"
)
DH_PRIME = bytes.fromhex(DH_PRIME_HEX.replace(" ", ""))

# Открытые ключи — из PEM Telegram Desktop (см. core/src/rsa_keys.inc); здесь только
# модуль в hex, чтобы эталон не зависел от C++-таблицы. Отпечаток пересчитывается.
RSA_KEYS_PEM = {
    "prod": """-----BEGIN RSA PUBLIC KEY-----
MIIBCgKCAQEA6LszBcC1LGzyr992NzE0ieY+BSaOW622Aa9Bd4ZHLl+TuFQ4lo4g
5nKaMBwK/BIb9xUfg0Q29/2mgIR6Zr9krM7HjuIcCzFvDtr+L0GQjae9H0pRB2OO
62cECs5HKhT5DZ98K33vmWiLowc621dQuwKWSQKjWf50XYFw42h21P2KXUGyp2y/
+aEyZ+uVgLLQbRA1dEjSDZ2iGRy12Mk5gpYc397aYp438fsJoHIgJ2lgMv5h7WY9
t6N/byY9Nw9p21Og3AoXSL2q/2IJ1WRUhebgAdGVMlV1fkuOQoEzR7EdpqtQD9Cs
5+bfo3Nhmcyvk5ftB0WkJ9z6bNZ7yxrP8wIDAQAB
-----END RSA PUBLIC KEY-----""",
    "test": """-----BEGIN RSA PUBLIC KEY-----
MIIBCgKCAQEAyMEdY1aR+sCR3ZSJrtztKTKqigvO/vBfqACJLZtS7QMgCGXJ6XIR
yy7mx66W0/sOFa7/1mAZtEoIokDP3ShoqF4fVNb6XeqgQfaUHd8wJpDWHcR2OFwv
plUUI1PLTktZ9uW2WE23b+ixNwJjJGwBDJPQEQFBE+vfmH0JP503wr5INS1poWg/
j25sIWeYPHYeOrFp/eXaqhISP6G+q2IeTaWTXpwZj4LzXq5YOpk4bYEQ6mvRq7D1
aHWfYmlEGepfaYR8Q0YqvvhYtMte3ITnuSJs171+GDqpdKcSwHnd6FudwGO4pcCO
j4WcDuXc2CTHgH8gFTNhp/Y8/SpDOhvn9QIDAQAB
-----END RSA PUBLIC KEY-----""",
}


# ------------------------------------------------------------------ примитивы

def aes_ecb(key: bytes, block: bytes, enc: bool) -> bytes:
    c = Cipher(algorithms.AES(key), modes.ECB())
    op = c.encryptor() if enc else c.decryptor()
    return op.update(block) + op.finalize()


def xor(a: bytes, b: bytes) -> bytes:
    return bytes(x ^ y for x, y in zip(a, b))


def ige(key: bytes, iv: bytes, data: bytes, enc: bool) -> bytes:
    # IGE: c_i = E(p_i ^ c_{i-1}) ^ p_{i-1}; iv = c_{-1} || p_{-1}
    c_prev, p_prev = iv[:16], iv[16:32]
    out = b""
    for i in range(0, len(data), 16):
        blk = data[i:i + 16]
        if enc:
            c = xor(aes_ecb(key, xor(blk, c_prev), True), p_prev)
            out += c
            c_prev, p_prev = c, blk
        else:
            p = xor(aes_ecb(key, xor(blk, p_prev), False), c_prev)
            out += p
            c_prev, p_prev = blk, p
    return out


def tl_bytes(b: bytes) -> bytes:
    r = bytes([len(b)]) + b if len(b) < 254 else b"\xfe" + len(b).to_bytes(3, "little") + b
    return r + b"\0" * (-len(r) % 4)


def key_id(auth_key: bytes) -> int:
    return int.from_bytes(sha1(auth_key)[-8:], "little")  # 64 младших бита SHA1


# ---- конверт MTProto 2.0 ----

def env_keys(auth_key: bytes, msg_key: bytes, x: int):
    a = sha256(msg_key + auth_key[x:x + 36])
    b = sha256(auth_key[40 + x:40 + x + 36] + msg_key)
    return a[:8] + b[8:24] + a[24:32], b[:8] + a[8:24] + b[24:32]


def env_seal(x, auth_key, salt, sess, msg_id, seq, body, pad):
    plaintext = (salt.to_bytes(8, "little") + sess.to_bytes(8, "little") + msg_id.to_bytes(8, "little")
                 + seq.to_bytes(4, "little") + len(body).to_bytes(4, "little") + body + pad)
    msg_key = sha256(auth_key[88 + x:88 + x + 32] + plaintext)[8:24]
    k, iv = env_keys(auth_key, msg_key, x)
    return key_id(auth_key).to_bytes(8, "little") + msg_key + ige(k, iv, plaintext, True)


def env_open(x, auth_key, wire):
    if len(wire) < 24 + 32 + 12 or (len(wire) - 24) % 16:
        return None
    if int.from_bytes(wire[:8], "little") != key_id(auth_key):
        return None
    msg_key = wire[8:24]
    k, iv = env_keys(auth_key, msg_key, x)
    pt = ige(k, iv, wire[24:], False)
    if sha256(auth_key[88 + x:88 + x + 32] + pt)[8:24] != msg_key:
        return None
    n = int.from_bytes(pt[28:32], "little")
    pad = len(pt) - 32 - n
    if n % 4 or not 12 <= pad <= 1024:
        return None
    ints = [int.from_bytes(pt[o:o + 8], "little") for o in (0, 8, 16)]
    return ints + [int.from_bytes(pt[24:28], "little"), pt[32:32 + n]]


# ---- RSA_PAD ----

def rsa_numbers(pem: str):
    from cryptography.hazmat.primitives.serialization import load_pem_public_key
    k = load_pem_public_key(pem.encode()).public_numbers()
    return k.n, k.e


def rsa_fingerprint(n: int, e: int) -> int:
    nb = n.to_bytes(256, "big")
    eb = e.to_bytes((e.bit_length() + 7) // 8, "big")
    return int.from_bytes(sha1(tl_bytes(nb) + tl_bytes(eb))[-8:], "little")


def rsa_pad(data: bytes, pad: bytes, temp_key: bytes, n: int, e: int):
    data_with_padding = data + pad
    assert len(data_with_padding) == 192
    data_pad_reversed = data_with_padding[::-1]
    data_with_hash = data_pad_reversed + sha256(temp_key + data_with_padding)
    aes_encrypted = ige(temp_key, b"\0" * 32, data_with_hash, True)
    temp_key_xor = xor(temp_key, sha256(aes_encrypted))
    key_aes_encrypted = temp_key_xor + aes_encrypted
    m = int.from_bytes(key_aes_encrypted, "big")
    if m >= n:
        return None
    return pow(m, e, n).to_bytes(256, "big")


# ---- DH ----

def tmp_key(new_nonce: bytes, server_nonce: bytes):
    key = sha1(new_nonce + server_nonce) + sha1(server_nonce + new_nonce)[:12]
    iv = sha1(server_nonce + new_nonce)[12:20] + sha1(new_nonce + new_nonce) + new_nonce[:4]
    return key, iv


def nonce_hash(new_nonce: bytes, auth_key: bytes, n: int) -> bytes:
    aux = sha1(auth_key)[:8]  # 64 старших бита
    return sha1(new_nonce + bytes([n]) + aux)[-16:]  # 128 младших бит


# ---- SRP ----

def srp(pw, salt1, salt2, g, p_bytes, gb_bytes, a_bytes):
    H = sha256
    SH = lambda d, s: H(s + d + s)
    ph1 = SH(SH(pw, salt1), salt2)
    ph2 = SH(hashlib.pbkdf2_hmac("sha512", ph1, salt1, 100000), salt2)
    p = int.from_bytes(p_bytes, "big")
    pad = lambda v: v.to_bytes(256, "big")
    g_pad = pad(g)
    x = int.from_bytes(ph2, "big")
    k = int.from_bytes(H(p_bytes + g_pad), "big")
    a = int.from_bytes(a_bytes, "big")
    g_a = pow(g, a, p)
    g_b = int.from_bytes(gb_bytes, "big")
    u = int.from_bytes(H(pad(g_a) + pad(g_b)), "big")
    v = pow(g, x, p)
    k_v = (k * v) % p
    t = (g_b - k_v) % p
    s_a = pow(t, a + u * x, p)
    k_a = H(pad(s_a))
    m1 = H(xor(H(p_bytes), H(g_pad)) + H(salt1) + H(salt2) + pad(g_a) + pad(g_b) + k_a)
    return pad(g_a), m1, (x, v, k_v, u)


# ---- obfuscated2 ----

def ctr(key: bytes, iv: bytes):
    return Cipher(algorithms.AES(key), modes.CTR(iv)).encryptor()


def obfs(transport, dc, secret, init_rnd, payload):
    tag = {0: b"\xef" * 4, 1: b"\xee" * 4, 2: b"\xdd" * 4}[transport]
    init = bytearray(init_rnd)
    init[56:60] = tag
    if dc:
        init[60:62] = dc.to_bytes(2, "little", signed=True)
    init = bytes(init)
    rev = init[::-1]
    ek, ei, dk, di = init[8:40], init[40:56], rev[8:40], rev[40:56]
    if secret:
        s = secret[1:] if len(secret) == 17 else secret
        ek, dk = sha256(ek + s), sha256(dk + s)
    enc = ctr(ek, ei)
    encrypted = enc.update(init)
    final = init[:56] + encrypted[56:64]
    return final, enc.update(payload), (dk, di)


# ------------------------------------------------------------------ сверка

class Core:
    def __init__(self, path):
        self.p = subprocess.Popen([path], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)

    def __call__(self, *args) -> str:
        line = " ".join(a.hex() if isinstance(a, (bytes, bytearray)) and a else ("-" if isinstance(a, (bytes, bytearray)) else str(a)) for a in args)
        self.p.stdin.write(line + "\n")
        self.p.stdin.flush()
        return self.p.stdout.readline().strip()

    def close(self):
        self.p.stdin.close()
        self.p.wait()


checks = 0
failures = []


def check(cond, what):
    global checks
    checks += 1
    if not cond:
        failures.append(what)


def main():
    core = Core(sys.argv[1])
    N = int(sys.argv[2]) if len(sys.argv) > 2 else 200
    R = random.SystemRandom()

    # Встроенное в C++ простое совпадает с текстом документации.
    check(len(DH_PRIME) == 256, "длина dh_prime")
    check(core("known_prime") == DH_PRIME.hex(), "встроенный dh_prime ядра совпадает с документацией")

    # Ключи: отпечатки из PEM — как опубликованы.
    keys = {}
    for name, pem in RSA_KEYS_PEM.items():
        n, e = rsa_numbers(pem)
        keys[name] = (rsa_fingerprint(n, e), n, e)
    check(keys["prod"][0] == 0xd09d1d85de64fd85, "отпечаток боевого ключа")
    check(keys["test"][0] == 0xb25898df208d2603, "отпечаток тестового ключа")
    for name, (fp, n, _) in keys.items():
        check(core("rsa_key", format(fp, "x")) == n.to_bytes(256, "big").hex(), f"модуль ключа {name} в таблице ядра")

    for i in range(N):
        # IGE
        klen = R.choice([16, 32])
        key, iv, data = rnd(klen), rnd(32), rnd(16 * R.randrange(0, 40))
        ct = ige(key, iv, data, True)
        check(core("ige_enc", key, iv, data) == (ct.hex() or "-"), f"ige_enc #{i}")
        check(core("ige_dec", key, iv, ct) == (data.hex() or "-"), f"ige_dec #{i}")

        # auth_key_id
        ak = rnd(256)
        check(core("keyid", ak) == str(key_id(ak)), f"keyid #{i}")

        # Конверт в обе стороны
        x = R.choice([0, 8])
        salt, sess, mid = R.getrandbits(64), R.getrandbits(64), R.getrandbits(64)
        seq = R.getrandbits(32)
        body = rnd(4 * R.randrange(0, 200))
        base = 32 + len(body)
        padlen = 12 + (-(base + 12) % 16) + 16 * R.randrange(0, 20)
        pad = rnd(padlen)
        wire = env_seal(x, ak, salt, sess, mid, seq, body, pad)
        check(core("env_seal", x, ak, salt, sess, mid, seq, body, pad) == wire.hex(), f"env_seal #{i}")
        opened = core("env_open", x, ak, wire).split()
        check(opened == [str(salt), str(sess), str(mid), str(seq), body.hex() or "-"], f"env_open #{i}")
        bad = bytearray(wire)
        bad[R.randrange(len(bad))] ^= 1 << R.randrange(8)
        check(core("env_open", x, ak, bytes(bad)).startswith("ERR") and env_open(x, ak, bytes(bad)) is None,
              f"env_open порча #{i}")

        # RSA_PAD с детерминированными temp_key и дополнением
        name = R.choice(["prod", "test"])
        fp, n, e = keys[name]
        d = rnd(R.randrange(0, 145))
        rp_pad, tk = rnd(192 - len(d)), rnd(32)
        expect = rsa_pad(d, rp_pad, tk, n, e)
        got = core("rsa_pad", format(fp, "x"), d, rp_pad, tk)
        check(got == (expect.hex() if expect else "ERR 6"), f"rsa_pad #{i}")

        # tmp_aes_key/iv и шифрование DH-сообщений
        nn, sn = rnd(32), rnd(16)
        k_, iv_ = tmp_key(nn, sn)
        check(core("tmp_key", nn, sn) == f"{k_.hex()} {iv_.hex()}", f"tmp_key #{i}")
        inner = rnd(4 * R.randrange(1, 150))
        p_ = rnd((-(20 + len(inner))) % 16)
        answer_with_hash = sha1(inner) + inner + p_
        enc = ige(k_, iv_, answer_with_hash, True)
        check(core("enc_dh", nn, sn, inner, p_) == enc.hex(), f"enc_dh #{i}")
        check(core("dec_dh", nn, sn, enc) == inner.hex(), f"dec_dh #{i}")

        # pq
        def prime(bits):
            while True:
                c = R.getrandbits(bits) | (1 << (bits - 1)) | 1
                if all(pow(w, c - 1, c) == 1 for w in (2, 3, 5, 7, 11, 13, 17, 19, 23, 29, 31, 37)):
                    return c
        pp, qq = sorted([prime(31 + R.randrange(2)), prime(31 + R.randrange(2))])
        check(core("pq", pp * qq) == f"{pp} {qq}", f"pq #{i}")

    # Тяжёлые случаи: DH и SRP (с серверной стороной), обфускация.
    P = int.from_bytes(DH_PRIME, "big")
    for i in range(max(10, N // 10)):
        nn, sn = rnd(32), rnd(16)
        a = R.getrandbits(2048)
        g = 3  # p mod 3 = 2 (для g = 2 нужно p mod 8 = 7, у этого простого 3)
        g_a = pow(g, a, P).to_bytes(256, "big")
        b = rnd(256)
        out = core("dh", nn, sn, DH_PRIME, g, g_a, b).split()
        g_b = pow(g, int.from_bytes(b, "big"), P).to_bytes(256, "big")
        client_key = pow(int.from_bytes(g_a, "big"), int.from_bytes(b, "big"), P).to_bytes(256, "big")
        server_key = pow(int.from_bytes(g_b, "big"), a, P).to_bytes(256, "big")
        check(client_key == server_key, f"dh ключи сторон #{i}")
        expect = [g_b.hex(), client_key.hex()] + [nonce_hash(nn, client_key, n).hex() for n in (1, 2, 3)] + \
                 [str(int.from_bytes(sha1(client_key)[:8], "little"))]
        check(out == expect, f"dh #{i}")

    for i in range(max(5, N // 40)):
        pw = rnd(R.randrange(1, 40))
        s1, s2 = rnd(R.randrange(8, 64)), rnd(16)
        g = 4  # g = 4 допустим для любого безопасного p (условия на p mod нет)
        # Сервер: v = g^x (x из того же пароля), b, g_b = k_v + g^b.
        _, _, (x, v, k_v, _) = srp(pw, s1, s2, g, DH_PRIME, (2).to_bytes(256, "big"), (2).to_bytes(256, "big"))
        b = R.getrandbits(2048)
        g_b = ((k_v + pow(g, b, P)) % P).to_bytes(256, "big")
        a = rnd(256)
        A, M1, (_, _, _, u) = srp(pw, s1, s2, g, DH_PRIME, g_b, a)
        check(core("srp", pw, s1, s2, g, DH_PRIME, g_b, a) == f"{A.hex()} {M1.hex()}", f"srp #{i}")
        # Сервер вычисляет M2 и должен получить M1.
        g_a = int.from_bytes(A, "big")
        s_b = pow(g_a * pow(v, u, P) % P, b, P)
        H = sha256
        g_pad = g.to_bytes(256, "big")
        M2 = H(xor(H(DH_PRIME), H(g_pad)) + H(s1) + H(s2) + A + g_b + H(s_b.to_bytes(256, "big")))
        check(M1 == M2, f"srp M1 == M2 #{i}")

    for i in range(N):
        transport = R.randrange(3)
        dc = R.choice([0, 2, -4, 10003])
        secret = R.choice([b"", rnd(16), b"\xdd" + rnd(16)])
        while True:
            ir = bytearray(rnd(64))
            if ir[0] == 0xef or bytes(ir[:4]) in (b"HEAD", b"POST", b"GET ", b"OPTI", b"\x16\x03\x01\x02",
                                                   b"\xdd" * 4, b"\xee" * 4) or bytes(ir[4:8]) == b"\0" * 4:
                continue
            break
        payload = rnd(R.randrange(0, 500))
        final, enc_payload, _ = obfs(transport, dc, secret, bytes(ir), payload)
        got = core("obfs", transport, dc, secret, bytes(ir), payload)
        check(got == f"{final.hex()} {enc_payload.hex() or '-'}", f"obfs #{i}")

    core.close()
    print(f"эталон: проверок {checks}, расхождений {len(failures)}")
    for f in failures[:20]:
        print("  расхождение:", f)
    sys.exit(1 if failures else 0)


if __name__ == "__main__":
    main()
