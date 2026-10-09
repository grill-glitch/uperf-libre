#!/usr/bin/env python3
"""Decrypt the obfuscated symbol-name table inside vendor `libsfanalysis.so`.

The vendor ships its four hook-target symbol names as a custom TEA-variant
cipher in `.data`; `.rodata` only carries the decoy `xh_refresh_loop` string
(which is xHook's *internal thread name*, not a hook target). Without
decrypting these you cannot know what the library actually hooks.

Findings (dev-22.09.04, sha256
386905b5e6237af09f61628f3a4feb257e17f47722a36b1c9a38eb0688987f31):

    ioctl
    epoll_wait
    pthread_cond_timedwait
    pthread_cond_wait

Record layout in `.data`:
    [u32 count ^ 0x9e377233][count*8 bytes ciphertext][0x00]

Cipher (per 8-byte pair, decrypt direction):
    key    = [0x000000e9, 0x0000091d, 0x00005b25, 0x00038f75]
    w16, w17 = 0x28b7bd67, 0xc6ef3720      # w17 = 0x9e3779b9 * 32
    repeat 32 times:
        w0 = ((v0 << 4) ^ (v0 >> 5)) + v0
        w0 ^= w17 + key[(w17 >> 11) & 3]
        v1 -= w0
        w0 = ((v1 << 4) ^ (v1 >> 5)) + v1
        w0 ^= w16 + key[w16 & 3]
        v0 -= w0
        w17 += 0x61c88647                  # -0x9e3779b9 mod 2**32
        w16 += 0x61c88647

Usage:
    python3 scripts/sfanalysis-deobf.py /path/to/libsfanalysis.so
"""

from __future__ import annotations

import struct
import sys

MASK = 0xFFFFFFFF
DELTA_NEG = 0x61C88647
KEY = [0x000000E9, 0x0000091D, 0x00005B25, 0x00038F75]
COUNT_XOR = 0x9E377233
TAG = b"\x72\x37\x9e"  # little-endian tail of the count word 0x9e3772XX


def decrypt_pair(v0: int, v1: int, rounds: int = 32) -> tuple[int, int]:
    w16, w17 = 0x28B7BD67, 0xC6EF3720
    for _ in range(rounds):
        w0 = (((v0 << 4) & MASK) ^ (v0 >> 5)) + v0 & MASK
        w0 ^= (w17 + KEY[(w17 >> 11) & 3]) & MASK
        v1 = (v1 - w0) & MASK
        w0 = (((v1 << 4) & MASK) ^ (v1 >> 5)) + v1 & MASK
        w0 ^= (w16 + KEY[w16 & 3]) & MASK
        v0 = (v0 - w0) & MASK
        w17 = (w17 + DELTA_NEG) & MASK
        w16 = (w16 + DELTA_NEG) & MASK
    return v0, v1


def extract_records(blob: bytes) -> list[tuple[int, bytes]]:
    """Return [(count, ciphertext), ...] for every record found in the blob."""
    out = []
    for i in range(len(blob) - 4):
        if blob[i + 1 : i + 4] != TAG:
            continue
        count_word = struct.unpack_from("<I", blob, i)[0]
        if (count_word >> 24) != 0x9E:
            continue
        count = count_word ^ COUNT_XOR
        start = i + 4
        end = start + count * 8
        if not (0 < count <= 8) or end > len(blob):
            continue
        out.append((count, blob[start:end]))
    return out


def decrypt_record(count: int, ct: bytes) -> bytes:
    plain = bytearray()
    for j in range(0, count * 2, 2):
        v0, v1 = struct.unpack_from("<II", ct, j * 4)
        d0, d1 = decrypt_pair(v0, v1)
        plain += struct.pack("<II", d0, d1)
    return bytes(plain).split(b"\x00", 1)[0]


def main() -> int:
    if len(sys.argv) != 2:
        print(__doc__)
        return 2
    blob = open(sys.argv[1], "rb").read()
    recs = extract_records(blob)
    if not recs:
        print("no obfuscated records found (not a v3 libsfanalysis.so?)")
        return 1
    print(f"{len(recs)} record(s) in {sys.argv[1]}")
    for count, ct in recs:
        name = decrypt_record(count, ct)
        try:
            shown = name.decode("utf-8")
        except UnicodeDecodeError:
            shown = name.hex()
        print(f"  count={count:<2} -> {shown!r}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
