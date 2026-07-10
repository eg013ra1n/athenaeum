#!/usr/bin/env python3
"""Generate Perseus icon assets from scratch — stdlib only (zlib + struct).

Draws the same anti-aliased filled circle the tray uses (indigo #4a5fd9 on a
transparent field) with a smaller white inner dot, legible down to 16px. Writes:

  crates/perseus/packaging/icon.png         1024x1024 RGBA master
  crates/perseus/packaging/icon.ico         ICO with embedded-PNG entries 256/64/32/16
  crates/perseus/packaging/linux/perseus.png 256x256 RGBA

Output is deterministic: no timestamp chunks, fixed zlib level — running twice
produces byte-identical files. The generated assets are committed alongside this
script; re-run only when the design changes.
"""
import math
import os
import struct
import zlib

INDIGO = (0x4A, 0x5F, 0xD9)  # #4a5fd9, matches the tray glyph accent
WHITE = (0xFF, 0xFF, 0xFF)
OUTER_R = 0.46  # disc radius as a fraction of the canvas
INNER_R = 0.17  # white dot radius as a fraction of the canvas

HERE = os.path.dirname(os.path.abspath(__file__))
PKG = os.path.normpath(os.path.join(HERE, "..", "crates", "perseus", "packaging"))


def render(n: int) -> bytes:
    """RGBA bytes (straight alpha) for an n x n icon: indigo disc + white dot."""
    c = (n - 1) / 2.0
    outer = OUTER_R * n
    inner = INNER_R * n
    buf = bytearray(n * n * 4)
    for y in range(n):
        for x in range(n):
            d = math.hypot(x - c, y - c)
            a = min(max(outer - d + 0.5, 0.0), 1.0)  # 1px anti-aliased edge
            i = (y * n + x) * 4
            if a <= 0.0:
                continue  # leave transparent
            w = min(max(inner - d + 0.5, 0.0), 1.0)  # white-dot coverage
            buf[i] = round(INDIGO[0] * (1 - w) + WHITE[0] * w)
            buf[i + 1] = round(INDIGO[1] * (1 - w) + WHITE[1] * w)
            buf[i + 2] = round(INDIGO[2] * (1 - w) + WHITE[2] * w)
            buf[i + 3] = round(a * 255)
    return bytes(buf)


def _chunk(tag: bytes, data: bytes) -> bytes:
    return struct.pack(">I", len(data)) + tag + data + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)


def png_bytes(n: int, rgba: bytes) -> bytes:
    """Encode an n x n RGBA buffer as a PNG (8-bit, color type 6, one IDAT)."""
    raw = bytearray()
    stride = n * 4
    for y in range(n):
        raw.append(0)  # filter type 0 (None)
        raw += rgba[y * stride:(y + 1) * stride]
    ihdr = struct.pack(">IIBBBBB", n, n, 8, 6, 0, 0, 0)
    idat = zlib.compress(bytes(raw), 9)
    return b"\x89PNG\r\n\x1a\n" + _chunk(b"IHDR", ihdr) + _chunk(b"IDAT", idat) + _chunk(b"IEND", b"")


def ico_bytes(sizes) -> bytes:
    """ICO container whose directory entries each point at an embedded PNG blob."""
    blobs = [png_bytes(s, render(s)) for s in sizes]
    header = struct.pack("<HHH", 0, 1, len(sizes))
    offset = 6 + 16 * len(sizes)
    entries = bytearray()
    for s, blob in zip(sizes, blobs):
        b = s % 256  # 256 encodes as 0
        entries += struct.pack("<BBBBHHII", b, b, 0, 0, 1, 32, len(blob), offset)
        offset += len(blob)
    return bytes(header) + bytes(entries) + b"".join(blobs)


def main() -> None:
    os.makedirs(os.path.join(PKG, "linux"), exist_ok=True)
    with open(os.path.join(PKG, "icon.png"), "wb") as f:
        f.write(png_bytes(1024, render(1024)))
    with open(os.path.join(PKG, "icon.ico"), "wb") as f:
        f.write(ico_bytes([256, 64, 32, 16]))
    with open(os.path.join(PKG, "linux", "perseus.png"), "wb") as f:
        f.write(png_bytes(256, render(256)))
    print("wrote icon.png (1024), icon.ico (256/64/32/16), linux/perseus.png (256)")


if __name__ == "__main__":
    main()
