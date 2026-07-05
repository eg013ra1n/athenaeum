#!/usr/bin/env python3
"""Compare two calibrated frames (Athenaeum FITS vs WBPP/Siril XISF or FITS).

Usage: compare_calibrated.py <a.fits|a.xisf> <b.fits|b.xisf>

Reads both images (float32 mono), dumps calibration-relevant metadata, then
quantifies how they differ: global affine relation (b ~ gain*a + offset),
residual structure (vignetting-like → flat mismatch; flat offset → pedestal /
dark mismatch), and block maps to localize structure. No deps beyond numpy.
"""
import re
import sys
import zlib
import numpy as np


# ── FITS ─────────────────────────────────────────────────────────────────────

def read_fits(path):
    cards = {}
    with open(path, "rb") as f:
        header_done = False
        while not header_done:
            block = f.read(2880)
            if len(block) < 2880:
                raise ValueError("truncated FITS header")
            for i in range(0, 2880, 80):
                card = block[i:i + 80].decode("ascii", "replace")
                kw = card[:8].strip()
                if kw == "END":
                    header_done = True
                    break
                if card[8:10] == "= " or kw in ("COMMENT", "HISTORY", "CONTINUE"):
                    cards[kw] = card[10:].split(" / ")[0].strip().strip("'").strip()
        bitpix = int(cards["BITPIX"])
        w, h = int(cards["NAXIS1"]), int(cards["NAXIS2"])
        if bitpix != -32:
            raise ValueError(f"expected float32 FITS, got BITPIX={bitpix}")
        n = w * h
        data = np.frombuffer(f.read(n * 4), dtype=">f4").astype(np.float32)
        if data.size != n:
            raise ValueError("truncated FITS data")
        bzero = float(cards.get("BZERO", 0) or 0)
        bscale = float(cards.get("BSCALE", 1) or 1)
        if (bzero, bscale) != (0.0, 1.0):
            data = data * bscale + bzero
    return data.reshape(h, w), cards


# ── XISF (monolithic, per XISF 1.0) ──────────────────────────────────────────

def read_xisf(path):
    with open(path, "rb") as f:
        sig = f.read(8)
        if sig != b"XISF0100":
            raise ValueError(f"not a monolithic XISF: {sig!r}")
        hlen = int.from_bytes(f.read(4), "little")
        f.read(4)  # reserved
        xml = f.read(hlen).decode("utf-8", "replace")
        m = re.search(r"<Image\b[^>]*>", xml)
        if not m:
            raise ValueError("no <Image> element")
        tag = m.group(0)

        def attr(name):
            am = re.search(rf'{name}="([^"]*)"', tag)
            return am.group(1) if am else None

        geometry = attr("geometry")            # "W:H:C"
        fmt = attr("sampleFormat") or "Float32"
        location = attr("location")            # "attachment:offset:size"
        compression = attr("compression")      # e.g. "zlib:uncompressed-size"
        byteorder = attr("byteOrder") or "little"
        w, h, c = (int(x) for x in geometry.split(":"))
        if fmt != "Float32":
            raise ValueError(f"expected Float32 XISF, got {fmt}")
        if c != 1:
            raise ValueError(f"expected mono, got {c} channels")
        loc = location.split(":")
        if loc[0] != "attachment":
            raise ValueError(f"unsupported location {location}")
        offset, size = int(loc[1]), int(loc[2])
        f.seek(offset)
        raw = f.read(size)
        if compression:
            parts = compression.split(":")
            if parts[0].startswith("zlib"):
                raw = zlib.decompress(raw)
            else:
                raise ValueError(f"unsupported compression {compression}")
        dt = "<f4" if byteorder == "little" else ">f4"
        data = np.frombuffer(raw, dtype=dt).astype(np.float32)
        if data.size != w * h:
            raise ValueError(f"data size {data.size} != {w}x{h}")
    return data.reshape(h, w), xml


def read_any(path):
    if path.lower().endswith(".xisf"):
        img, meta = read_xisf(path)
        return img, {"xisf_xml": meta}
    img, cards = read_fits(path)
    return img, cards


# ── analysis ─────────────────────────────────────────────────────────────────

def stats(name, a):
    finite = np.isfinite(a)
    v = a[finite]
    print(f"\n[{name}] {a.shape[1]}x{a.shape[0]}")
    print(f"  min={v.min():.6g} p1={np.percentile(v,1):.6g} median={np.median(v):.6g} "
          f"mean={v.mean():.6g} p99={np.percentile(v,99):.6g} max={v.max():.6g}")
    print(f"  std={v.std():.6g}  neg={100*(v<0).mean():.3f}%  zero={100*(v==0).mean():.3f}%  "
          f"nonfinite={int((~finite).sum())}")


def block_map(a, blocks=6):
    h, w = a.shape
    bh, bw = h // blocks, w // blocks
    return np.array([[np.median(a[r*bh:(r+1)*bh, c*bw:(c+1)*bw])
                      for c in range(blocks)] for r in range(blocks)])


def print_grid(title, g, fmt="{:9.4f}"):
    print(f"  {title}")
    for row in g:
        print("    " + " ".join(fmt.format(x) for x in row))


def main():
    pa, pb = sys.argv[1], sys.argv[2]
    a, meta_a = read_any(pa)
    b, meta_b = read_any(pb)

    print("=== metadata A ===")
    if "xisf_xml" in meta_a:
        for kw in re.findall(r'<FITSKeyword name="(PEDESTAL|CALSTAT|IMAGETYP|EXPOSURE|EXPTIME)" value="([^"]*)"', meta_a["xisf_xml"]):
            print(f"  {kw[0]} = {kw[1]}")
    else:
        for k in ("CALSTAT", "ATH_CSCL", "ATH_CFNM", "ATH_CVER", "ATH_CDRK", "ATH_CFLT", "ATH_CBIA", "PEDESTAL", "BZERO", "BSCALE"):
            if k in meta_a:
                print(f"  {k} = {meta_a[k]}")
    print("=== metadata B ===")
    if "xisf_xml" in meta_b:
        for kw in re.findall(r'<FITSKeyword name="(PEDESTAL|CALSTAT|IMAGETYP|EXPOSURE|EXPTIME)" value="([^"]*)"[^/]*/>', meta_b["xisf_xml"]):
            print(f"  {kw[0]} = {kw[1]}")
        for prop in re.findall(r'<Property id="(PixInsight:[^"]*)"[^>]*value="([^"]{0,60})"', meta_b["xisf_xml"])[:10]:
            print(f"  {prop[0]} = {prop[1]}")
    else:
        for k in ("CALSTAT", "ATH_CSCL", "ATH_CFNM", "ATH_CVER", "ATH_CDRK", "ATH_CFLT", "ATH_CBIA", "PEDESTAL", "BZERO", "BSCALE"):
            if k in meta_b:
                print(f"  {k} = {meta_b[k]}")

    if a.shape != b.shape:
        print(f"\nSHAPE MISMATCH: {a.shape} vs {b.shape} — cannot compare per-pixel")
        stats("A " + pa.rsplit('/', 1)[-1], a)
        stats("B " + pb.rsplit('/', 1)[-1], b)
        return

    stats("A (athenaeum?)", a)
    stats("B (wbpp?)", b)

    ok = np.isfinite(a) & np.isfinite(b)
    af, bf = a[ok].astype(np.float64), b[ok].astype(np.float64)

    # Global affine fit b = gain*a + offset (robust: sample + 2-pass sigma clip)
    rng = np.random.default_rng(42)
    idx = rng.choice(af.size, size=min(500_000, af.size), replace=False)
    xs, ys = af[idx], bf[idx]
    for _ in range(2):
        gain, offset = np.polyfit(xs, ys, 1)
        r = ys - (gain * xs + offset)
        keep = np.abs(r) < 4 * r.std()
        xs, ys = xs[keep], ys[keep]
    gain, offset = np.polyfit(xs, ys, 1)
    corr = np.corrcoef(xs, ys)[0, 1]
    resid = ys - (gain * xs + offset)

    print("\n=== relation B ~ gain*A + offset (robust fit) ===")
    print(f"  gain   = {gain:.6f}")
    print(f"  offset = {offset:.6g}  ({offset*65535:.2f} DN @16bit)")
    print(f"  corr   = {corr:.6f}")
    print(f"  residual std = {resid.std():.4g}  (vs B std {bf.std():.4g} → "
          f"{100*resid.std()/bf.std():.1f}% unexplained)")
    med_ratio = np.median(bf[af > np.percentile(af, 10)] / np.maximum(af[af > np.percentile(af, 10)], 1e-12))
    print(f"  median(B/A) over bright half = {med_ratio:.6f}")

    # Structure: block maps
    print("\n=== spatial structure (6x6 block medians) ===")
    print_grid("A:", block_map(a))
    print_grid("B:", block_map(b))
    with np.errstate(divide="ignore", invalid="ignore"):
        ratio = np.where(np.abs(a) > 1e-6, b / a, np.nan)
    print_grid("B/A ratio:", block_map(np.nan_to_num(ratio, nan=np.nanmedian(ratio))))
    print_grid("B - (gain*A+offset) residual:", block_map(b - (gain * a + offset).astype(np.float32)), fmt="{:10.3e}")

    # Interpretation hints
    print("\n=== hints ===")
    rmap = block_map(np.nan_to_num(ratio, nan=np.nanmedian(ratio)))
    center = rmap[2:4, 2:4].mean()
    corners = (rmap[0, 0] + rmap[0, -1] + rmap[-1, 0] + rmap[-1, -1]) / 4
    print(f"  ratio center={center:.4f} corners={corners:.4f} corner/center={corners/center:.4f}")
    print("  corner/center far from 1.0 → flat-field disagreement (vignetting residue)")
    print("  |offset| >> 0 with gain ~const → pedestal / dark-level disagreement")
    print("  gain far from 1.0 with offset ~0 → pure scale (normalization) difference")


if __name__ == "__main__":
    main()
