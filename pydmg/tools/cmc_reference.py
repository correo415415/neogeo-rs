#!/usr/bin/env python3
"""Independent Python reference implementation of MAME's CMC42/CMC50
gfx_decrypt / sfix_decrypt / cmc50_m1_decrypt, transcribed directly from
prot_cmc.cpp. Used to generate golden SHA-256 hashes for the Rust port's
differential tests (no ROMs required).

    python3 tools/cmc_reference.py <path-to-prot_cmc.cpp>
"""
import hashlib
import re
import sys


def load_tables(path):
    src = open(path, encoding="utf-8").read()
    tables = {}
    for name, body in re.findall(
        r"static const uint8_t (\w+)\[256\]\s*=\s*\{(.*?)\};", src, re.S
    ):
        body = re.sub(r"//[^\n]*", "", body)
        vals = [int(v, 16) for v in re.findall(r"0x([0-9a-fA-F]{2})", body)]
        assert len(vals) == 256, name
        tables[name] = vals
    return tables


def decrypt(c0, c1, t0hi, t0lo, t1, a07, base, invert):
    tmp = t1[(base & 0xFF) ^ a07[(base >> 8) & 0xFF]]
    xor0 = (t0hi[(base >> 8) & 0xFF] & 0xFE) | (tmp & 0x01)
    xor1 = (tmp & 0xFE) | (t0lo[(base >> 8) & 0xFF] & 0x01)
    if invert:
        return c1 ^ xor0, c0 ^ xor1
    return c0 ^ xor0, c1 ^ xor1


def gfx_decrypt(T, prefix, rom, extra_xor):
    t0_03 = T[f"{prefix}_type0_t03"]
    t0_12 = T[f"{prefix}_type0_t12"]
    t1_03 = T[f"{prefix}_type1_t03"]
    t1_12 = T[f"{prefix}_type1_t12"]
    a815x1 = T[f"{prefix}_address_8_15_xor1"]
    a815x2 = T[f"{prefix}_address_8_15_xor2"]
    a1623x1 = T[f"{prefix}_address_16_23_xor1"]
    a1623x2 = T[f"{prefix}_address_16_23_xor2"]
    a07 = T[f"{prefix}_address_0_7_xor"]

    n = len(rom)
    words = n // 4
    buf = bytearray(n)
    for rpos in range(words):
        b = 4 * rpos
        r0, r3 = decrypt(rom[b], rom[b + 3], t0_03, t0_12, t1_03, a07,
                         rpos, (rpos >> 8) & 1)
        buf[b], buf[b + 3] = r0, r3
        inv2 = ((rpos >> 16) ^ a1623x2[(rpos >> 8) & 0xFF]) & 1
        r1, r2 = decrypt(rom[b + 1], rom[b + 2], t0_12, t0_03, t1_12, a07,
                         rpos, inv2)
        buf[b + 1], buf[b + 2] = r1, r2

    for rpos in range(words):
        baser = rpos ^ extra_xor
        baser ^= a815x1[(baser >> 16) & 0xFF] << 8
        baser ^= a815x2[baser & 0xFF] << 8
        baser ^= a1623x1[baser & 0xFF] << 16
        baser ^= a1623x2[(baser >> 8) & 0xFF] << 16
        baser ^= a07[(baser >> 8) & 0xFF]
        if n == 0x3000000:
            baser = baser & (0x2000000 // 4 - 1) if rpos < 0x2000000 // 4 \
                else 0x2000000 // 4 + (baser & (0x1000000 // 4 - 1))
        elif n == 0x6000000:
            baser = baser & (0x4000000 // 4 - 1) if rpos < 0x4000000 // 4 \
                else 0x4000000 // 4 + (baser & (0x1000000 // 4 - 1))
        else:
            baser &= words - 1
        rom[4 * rpos:4 * rpos + 4] = buf[4 * baser:4 * baser + 4]


def sfix_decrypt(rom, tx):
    src = rom[len(rom) - tx:]
    return bytes(
        src[(i & ~0x1F) + ((i & 7) << 2) + ((~i & 8) >> 2) + ((i & 0x10) >> 4)]
        for i in range(tx)
    )


def bitswap16(v, order):
    out = 0
    for n, b in enumerate(order):
        out |= ((v >> b) & 1) << (15 - n)
    return out


P1 = [
    [15, 14, 10, 7, 1, 2, 3, 8, 0, 12, 11, 13, 6, 9, 5, 4],
    [7, 1, 8, 11, 15, 9, 2, 3, 5, 13, 4, 14, 10, 0, 6, 12],
    [8, 6, 14, 3, 10, 7, 15, 1, 4, 0, 2, 5, 13, 11, 12, 9],
    [2, 8, 15, 9, 3, 4, 11, 7, 13, 6, 0, 10, 1, 12, 14, 5],
    [1, 13, 6, 15, 14, 3, 8, 10, 9, 4, 7, 12, 5, 2, 0, 11],
    [11, 15, 3, 4, 7, 0, 9, 2, 6, 14, 12, 1, 8, 5, 10, 13],
    [10, 5, 13, 8, 6, 15, 1, 14, 11, 9, 3, 0, 12, 7, 4, 2],
    [9, 3, 7, 0, 2, 12, 4, 11, 14, 10, 5, 8, 15, 13, 1, 6],
]


def m1_address_scramble(T, address, key):
    block = (address >> 16) & 7
    aux = address & 0xFFFF
    aux ^= bitswap16(key, [12, 0, 2, 4, 8, 15, 7, 13, 10, 1, 3, 6, 11, 9, 14, 5])
    aux = bitswap16(aux, list(reversed(P1[block])))
    aux ^= T["m1_address_0_7_xor"][(aux >> 8) & 0xFF]
    aux ^= T["m1_address_8_15_xor"][aux & 0xFF] << 8
    aux = bitswap16(aux, [7, 15, 14, 6, 5, 13, 12, 4, 11, 3, 10, 2, 9, 1, 8, 0])
    return (block << 16) | aux


def cmc50_m1_decrypt(T, rom):
    key = sum(rom[:0x10000]) & 0xFFFF
    return bytes(rom[m1_address_scramble(T, i, key)] for i in range(0x80000)), key


def prng(n):
    out = bytearray(n)
    for i in range(n):
        out[i] = ((i * 2654435761 + 12345) >> 7) & 0xFF
    return out


def main():
    T = load_tables(sys.argv[1])

    # CMC42 / mslug3 key on a 1 MiB pseudorandom region
    rom = prng(0x100000)
    gfx_decrypt(T, "kof99", rom, 0xAD)
    print("cmc42_gfx  :", hashlib.sha256(rom).hexdigest())
    s = sfix_decrypt(rom, 0x20000)
    print("cmc42_sfix :", hashlib.sha256(s).hexdigest())

    # CMC50 / mslug5 key
    rom = prng(0x100000)
    gfx_decrypt(T, "kof2000", rom, 0x19)
    print("cmc50_gfx  :", hashlib.sha256(rom).hexdigest())

    # CMC50 M1
    m1 = prng(0x80000)
    dec, key = cmc50_m1_decrypt(T, m1)
    print(f"m1_key     : {key:#06x}")
    print("cmc50_m1   :", hashlib.sha256(dec).hexdigest())


if __name__ == "__main__":
    main()
