#!/usr/bin/env python3
"""Compare two float32 images, FITS (BITPIX -32 primary HDU) or monolithic
XISF (Float32, planar, uncompressed attachment), pixel by pixel.

    imgcmp.py <a> <b> [--scale-b F]

Prints the shapes, the median / p99 / max absolute difference, the fraction of
pixels that differ by more than 1e-6, and the medians of both images. Exits 1
when the shapes differ. numpy only — no astropy on the dev Mac.
`--scale-b` multiplies B before comparing (e.g. 1/65535 when B carries ADU and
A is unit-scaled).
"""
import re
import sys

import numpy as np


def read_fits(path):
    with open(path, "rb") as f:
        hdr = b""
        while True:
            block = f.read(2880)
            if not block:
                raise ValueError("no END card")
            hdr += block
            cards = [hdr[i:i + 80] for i in range(0, len(hdr), 80)]
            if any(c.startswith(b"END     ") for c in cards):
                break
        keys = {}
        for i in range(0, len(hdr), 80):
            card = hdr[i:i + 80].decode("latin-1")
            kw = card[:8].strip()
            if kw == "END":
                break
            if card[8:10] == "= " and kw not in keys:
                keys[kw] = card[10:].split("/")[0].strip().strip("'").strip()
        def key(k):
            return keys.get(k)
        bitpix = int(key("BITPIX"))
        naxis = int(key("NAXIS"))
        dims = [int(key(f"NAXIS{i}")) for i in range(1, naxis + 1)]
        if bitpix != -32:
            raise ValueError(f"BITPIX {bitpix} not supported")
        n = int(np.prod(dims))
        data = np.frombuffer(f.read(n * 4), dtype=">f4").astype(np.float32)
    return data.reshape(dims[::-1])


def read_xisf(path):
    with open(path, "rb") as f:
        sig = f.read(8)
        if sig != b"XISF0100":
            raise ValueError("not a monolithic XISF")
        hlen = int.from_bytes(f.read(4), "little")
        f.read(4)
        xml = f.read(hlen).decode("utf-8", "replace")
        geom = re.search(r'geometry="(\d+):(\d+):(\d+)"', xml)
        loc = re.search(r'location="attachment:(\d+):(\d+)"', xml)
        fmt = re.search(r'sampleFormat="(\w+)"', xml).group(1)
        bounds = re.search(r'bounds="([^"]+)"', xml)
        if fmt != "Float32":
            raise ValueError(f"sampleFormat {fmt} not supported")
        w, h, c = (int(x) for x in geom.groups())
        pos, size = (int(x) for x in loc.groups())
        f.seek(pos)
        data = np.frombuffer(f.read(size), dtype="<f4").astype(np.float32)
    print(f"  xisf {path}: {w}x{h}x{c} bounds={bounds.group(1) if bounds else None}")
    return data.reshape((c, h, w)) if c > 1 else data.reshape((h, w))


def read(path):
    return read_xisf(path) if path.lower().endswith(".xisf") else read_fits(path)


def main():
    args = sys.argv[1:]
    scale_b = 1.0
    if "--scale-b" in args:
        i = args.index("--scale-b")
        scale_b = float(args[i + 1])
        del args[i:i + 2]
    a, b = read(args[0]), read(args[1]) * np.float32(scale_b)
    print(f"  a {a.shape}  b {b.shape}")
    if a.shape != b.shape:
        print("SHAPE MISMATCH")
        sys.exit(1)
    d = np.abs(a.astype(np.float64) - b.astype(np.float64))
    print(f"  median(a)={np.median(a):.6g} median(b)={np.median(b):.6g}")
    print(f"  |diff| median={np.median(d):.3g} p99={np.percentile(d, 99):.3g} max={d.max():.3g}")
    print(f"  pixels differing > 1e-6: {100.0 * np.mean(d > 1e-6):.4f} %")


if __name__ == "__main__":
    main()
