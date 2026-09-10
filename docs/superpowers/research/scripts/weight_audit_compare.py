#!/usr/bin/env python3
"""Compare `examples/weight_audit.rs` measurements against the external
tool's per-frame run log (M4a Task 1, ruling R-M4a-2 — the acceptance
instrument for the stacking pipeline's PSF-weight work). stdlib only.

Usage:
  weight_audit_compare.py --log <external.log> --ours <file.jsonl> \
      [--terms-out wbpp-terms.json]
  weight_audit_compare.py --dump-first-image <file.xisf> <out.bin>

The first form parses the external log's per-frame blocks and the
harness's JSONL output, joins them by file stem, and prints per-night
median ratio tables plus per-channel Spearman correlations and a
top-20-overlap check, each as a PASS/MISS line per ruling R-M4a-2's
targets (grep `^(PASS|MISS) `).

The second form is the Step 1 raw-bytes fallback: it reads an XISF
file's own signature + XML header, finds the FIRST `<Image>` element's
`location="attachment:pos:size"`, and writes those `size` raw bytes at
`pos` to `out.bin` verbatim (no conversion) — for diffing against
`weight_audit --dump-planes` when the two-image fixture test does not
reproduce the disagreement.
"""
import argparse
import json
import os
import re
import statistics
import struct
import sys

LOAD_RE = re.compile(r"\* Loading target (?:calibration frame|file): (.+)$")
PSF_RE = re.compile(
    r"ch (\d+) : TFlux = ([0-9.eE+-]+), TMeanFlux = ([0-9.eE+-]+), "
    r"M\* = ([0-9.eE+-]+), N\* = ([0-9.eE+-]+), (\d+) PSF fits"
)
NOISE_RE = re.compile(
    r"ch (\d+) : sigma_n = ([0-9.eE+-]+), ([0-9.]+)% pixels \((\w+)\)"
)

# The external log's `M* = a, TFlux = b, TMeanFlux = c, sigma_n = d` terms
# recomputed with our own PSF-signal-weight constants (ruling R-M4a-2);
# `psf_signal::psf_signal_weight` in athenaeum-core is the Rust source of
# truth this mirrors for the comparison only — never imported, this script
# has no Rust runtime to call into.
PSFSW_NUM = 5.326e-6
PSFSW_DEN = 9.0e6


def strip_suffix(stem, suffixes):
    """Strip the first suffix in `suffixes` (checked in order) that
    matches the end of `stem`."""
    for suf in suffixes:
        if stem.endswith(suf):
            return stem[: -len(suf)]
    return stem


def stem_of(path):
    return os.path.splitext(os.path.basename(path))[0]


def night_of(stem):
    m = re.match(r"(\d{4}-\d{2}-\d{2})", stem)
    return m.group(1) if m else "unknown"


def parse_external_log(path):
    """-> dict[stem, {"ch": {i: {tflux, tmean, mstar, nstar, fits, sigma}}}]

    A block starts at `* Loading target calibration frame: <path>` (mono —
    the pre-calibration source path, which carries no `_c` suffix at all)
    or `* Loading target file: <path>` (OSC — the ALREADY-calibrated `_c`
    CFA file being debayered); either way the stem is stripped of a
    trailing `_c` so mono and OSC stems land on the same base as
    `load_ours`'s post-strip stems.
    """
    result = {}
    current = None
    with open(path, "r", encoding="utf-8", errors="replace") as f:
        for line in f:
            m = LOAD_RE.search(line)
            if m:
                stem = strip_suffix(stem_of(m.group(1).strip()), ["_c"])
                current = stem
                result.setdefault(current, {"ch": {}})
                continue
            if current is None:
                continue
            m = PSF_RE.search(line)
            if m:
                idx = int(m.group(1))
                ch = result[current]["ch"].setdefault(idx, {})
                ch["tflux"] = float(m.group(2))
                ch["tmean"] = float(m.group(3))
                ch["mstar"] = float(m.group(4))
                ch["nstar"] = float(m.group(5))
                ch["fits"] = int(m.group(6))
                continue
            m = NOISE_RE.search(line)
            if m:
                idx = int(m.group(1))
                ch = result[current]["ch"].setdefault(idx, {})
                ch["sigma"] = float(m.group(2))
    return result


def load_ours(jsonl_path):
    """-> dict[stem, channels] — `channels` is the JSONL line's `channels`
    array (each entry the serde JSON of a `ChannelMeasurement`), keyed by
    the stem with a trailing `_c_d` (OSC) or `_c` (mono) stripped so it
    joins against `parse_external_log`'s stems."""
    result = {}
    with open(jsonl_path, "r", encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            obj = json.loads(line)
            stem = strip_suffix(obj["stem"], ["_c_d", "_c"])
            result[stem] = obj["channels"]
    return result


def external_psf_signal_weight(ch):
    """Recompute PSFSW from the external log's own terms with our
    constants (ruling R-M4a-2)."""
    tflux = ch.get("tflux")
    tmean = ch.get("tmean")
    mstar = ch.get("mstar")
    sigma = ch.get("sigma")
    if None in (tflux, tmean, mstar, sigma) or mstar == 0 or sigma == 0:
        return None
    return PSFSW_NUM * tflux * tmean / (PSFSW_DEN * sigma * mstar)


def _rank(values):
    """Average ranks (1-based), ties split evenly."""
    order = sorted(range(len(values)), key=lambda i: values[i])
    ranks = [0.0] * len(values)
    i = 0
    while i < len(order):
        j = i
        while j + 1 < len(order) and values[order[j + 1]] == values[order[i]]:
            j += 1
        avg_rank = (i + j) / 2.0 + 1
        for k in range(i, j + 1):
            ranks[order[k]] = avg_rank
        i = j + 1
    return ranks


def spearman(xs, ys):
    """Spearman rank correlation, stdlib only. `None` below 3 points or a
    degenerate (zero-variance) series."""
    n = len(xs)
    if n < 3:
        return None
    rx = _rank(xs)
    ry = _rank(ys)
    mx = statistics.mean(rx)
    my = statistics.mean(ry)
    num = sum((a - mx) * (b - my) for a, b in zip(rx, ry))
    denx = sum((a - mx) ** 2 for a in rx) ** 0.5
    deny = sum((b - my) ** 2 for b in ry) ** 0.5
    if denx == 0 or deny == 0:
        return None
    return num / (denx * deny)


def _med(values):
    return statistics.median(values) if values else None


def _fmt(v):
    return f"{v:.4f}" if v is not None else "n/a"


def group_stems(ours):
    """mono = 1-channel stems, osc = 3-channel stems (anything else is
    reported but not graded — the harness should never emit it)."""
    groups = {"mono": [], "osc": []}
    other = []
    for stem, channels in ours.items():
        if len(channels) == 1:
            groups["mono"].append(stem)
        elif len(channels) == 3:
            groups["osc"].append(stem)
        else:
            other.append((stem, len(channels)))
    groups["mono"].sort()
    groups["osc"].sort()
    if other:
        print(
            f"# note: {len(other)} stem(s) with neither 1 nor 3 channels, "
            "skipped from grouping",
            file=sys.stderr,
        )
    return groups


def report(ext, ours):
    groups = group_stems(ours)
    pass_count = 0
    miss_count = 0

    def verdict(ok):
        nonlocal pass_count, miss_count
        if ok:
            pass_count += 1
            return "PASS"
        miss_count += 1
        return "MISS"

    for group_name, stems in groups.items():
        matched = [s for s in stems if s in ext]
        print(f"\n=== {group_name}: {len(matched)}/{len(stems)} frames matched the log ===")
        if not matched:
            continue
        n_channels = len(ours[matched[0]])
        nights = sorted({night_of(s) for s in matched})

        for ch_idx in range(n_channels):
            print(f"-- {group_name} channel {ch_idx} --")
            print(
                f"{'night':<12} {'n':>4} {'starsRatio':>11} {'tfluxRatio':>11} "
                f"{'tmeanRatio':>11} {'mStarRatio':>11} {'nStarRatio':>11} {'noiseRatio':>11}"
            )
            for night in nights:
                night_stems = [s for s in matched if night_of(s) == night]
                stars_r, tflux_r, tmean_r, mstar_r, nstar_r, noise_r = (
                    [],
                    [],
                    [],
                    [],
                    [],
                    [],
                )
                for s in night_stems:
                    if ch_idx >= len(ours[s]):
                        continue
                    oc = ours[s][ch_idx]
                    ec = ext[s]["ch"].get(ch_idx)
                    if ec is None:
                        continue
                    if ec.get("fits"):
                        stars_r.append(oc["starsFitted"] / ec["fits"])
                    if ec.get("tflux"):
                        tflux_r.append(oc["tflux"] / ec["tflux"])
                    if ec.get("tmean"):
                        tmean_r.append(oc["tmeanFlux"] / ec["tmean"])
                    if ec.get("mstar"):
                        mstar_r.append(oc["mStar"] / ec["mstar"])
                    if ec.get("nstar"):
                        nstar_r.append(oc["nStar"] / ec["nstar"])
                    if ec.get("sigma"):
                        noise_r.append(oc["noise"] / ec["sigma"])
                print(
                    f"{night:<12} {len(night_stems):>4} {_fmt(_med(stars_r)):>11} "
                    f"{_fmt(_med(tflux_r)):>11} {_fmt(_med(tmean_r)):>11} "
                    f"{_fmt(_med(mstar_r)):>11} {_fmt(_med(nstar_r)):>11} {_fmt(_med(noise_r)):>11}"
                )
                m = _med(stars_r)
                if m is not None:
                    ok = 0.7 <= m <= 1.4
                    print(
                        f"{verdict(ok)} fits_ratio_in_0.7_1.4 group={group_name} "
                        f"ch={ch_idx} night={night} ratio={m:.4f}"
                    )

            xs_psfsw_ours, xs_psfsw_ext = [], []
            xs_fits_ours, xs_fits_ext = [], []
            for s in matched:
                if ch_idx >= len(ours[s]):
                    continue
                oc = ours[s][ch_idx]
                ec = ext[s]["ch"].get(ch_idx)
                if ec is None:
                    continue
                psfsw = external_psf_signal_weight(ec)
                if psfsw is not None:
                    xs_psfsw_ours.append(oc["psfSignalWeight"])
                    xs_psfsw_ext.append(psfsw)
                if ec.get("fits") is not None:
                    xs_fits_ours.append(oc["starsFitted"])
                    xs_fits_ext.append(ec["fits"])

            sp_psfsw = spearman(xs_psfsw_ours, xs_psfsw_ext)
            if sp_psfsw is None:
                print(f"psfsw_spearman group={group_name} ch={ch_idx}: n/a (fewer than 3 frames)")
            else:
                print(
                    f"{verdict(sp_psfsw >= 0.90)} psfsw_spearman_ge_0.90 "
                    f"group={group_name} ch={ch_idx} rho={sp_psfsw:.4f}"
                )

            sp_fits = spearman(xs_fits_ours, xs_fits_ext)
            if sp_fits is None:
                print(f"fits_spearman group={group_name} ch={ch_idx}: n/a (fewer than 3 frames)")
            else:
                print(
                    f"{verdict(sp_fits >= 0.80)} fits_spearman_ge_0.80 "
                    f"group={group_name} ch={ch_idx} rho={sp_fits:.4f}"
                )

        # Top-20 overlap of the frame-mean normalized PSF Signal Weight.
        ours_mean, ext_mean = {}, {}
        for s in matched:
            vals = [c["psfSignalWeight"] for c in ours[s]]
            if vals:
                ours_mean[s] = sum(vals) / len(vals)
            evs = []
            for ch_idx in range(n_channels):
                ec = ext[s]["ch"].get(ch_idx)
                if ec is None:
                    continue
                v = external_psf_signal_weight(ec)
                if v is not None:
                    evs.append(v)
            if evs:
                ext_mean[s] = sum(evs) / len(evs)

        if len(ours_mean) >= 20 and len(ext_mean) >= 20:
            ours_max = max(ours_mean.values())
            ext_max = max(ext_mean.values())
            ours_norm = {
                s: v / ours_max for s, v in ours_mean.items() if ours_max > 0
            }
            ext_norm = {s: v / ext_max for s, v in ext_mean.items() if ext_max > 0}
            top_ours = set(sorted(ours_norm, key=lambda s: -ours_norm[s])[:20])
            top_ext = set(sorted(ext_norm, key=lambda s: -ext_norm[s])[:20])
            overlap = len(top_ours & top_ext)
            print(
                f"{verdict(overlap >= 14)} top20_overlap_ge_14 group={group_name} "
                f"overlap={overlap}/20"
            )
        else:
            print(
                f"top20_overlap group={group_name}: n/a "
                f"(fewer than 20 frames — ours={len(ours_mean)}, ext={len(ext_mean)})"
            )

    print(f"\n=== summary: {pass_count} PASS, {miss_count} MISS ===")


def dump_first_image(xisf_path, out_path):
    """Raw reader (Step 1's fallback characterisation): signature, XML
    length, the first `<Image>`'s `location="attachment:pos:size"`, read
    `size` bytes at `pos` — no conversion, no scaling, no bounds mapping."""
    with open(xisf_path, "rb") as f:
        sig = f.read(8)
        if sig != b"XISF0100":
            print(f"not an XISF file (bad signature): {xisf_path}", file=sys.stderr)
            sys.exit(1)
        (xml_len,) = struct.unpack("<I", f.read(4))
        f.read(4)  # reserved
        xml = f.read(xml_len).decode("utf-8", "replace")
        m = re.search(r"<Image\b[^>]*>", xml)
        if not m:
            print("no <Image> element found in header", file=sys.stderr)
            sys.exit(1)
        loc = re.search(r'location="attachment:(\d+):(\d+)"', m.group(0))
        if not loc:
            print("first <Image> has no attachment location", file=sys.stderr)
            sys.exit(1)
        pos, size = int(loc.group(1)), int(loc.group(2))
        f.seek(pos)
        raw = f.read(size)
    with open(out_path, "wb") as out:
        out.write(raw)
    print(f"wrote {len(raw)} raw bytes (attachment at {pos}, size {size}) to {out_path}")


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--log", help="external tool's per-frame run log")
    parser.add_argument("--ours", help="examples/weight_audit.rs JSONL output")
    parser.add_argument("--terms-out", help="dump the parsed external-log terms as JSON")
    parser.add_argument(
        "--dump-first-image",
        nargs=2,
        metavar=("XISF", "OUT"),
        help="Step 1 fallback: dump the first <Image>'s raw attachment bytes",
    )
    args = parser.parse_args()

    if args.dump_first_image:
        dump_first_image(*args.dump_first_image)
        return

    if not args.log or not args.ours:
        parser.error("--log and --ours are required (or use --dump-first-image)")

    ext = parse_external_log(args.log)
    ours = load_ours(args.ours)

    if args.terms_out:
        with open(args.terms_out, "w", encoding="utf-8") as f:
            json.dump(ext, f, indent=2, sort_keys=True)
        print(f"wrote {len(ext)} external-log frame(s) to {args.terms_out}", file=sys.stderr)

    report(ext, ours)


if __name__ == "__main__":
    main()
