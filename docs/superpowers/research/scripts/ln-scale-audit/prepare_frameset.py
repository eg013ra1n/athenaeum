#!/usr/bin/env python3
"""Frame-set inputs for ln_scale_probe (ln-scale-audit 2026-10-04).

    prepare_frameset.py <catalog-copy.db> <set-id> <run-id> <old-work-prefix> <new-work-prefix> [<group-key> <every-nth>]...

Reads a scratch copy of an acceptance catalog (never the live one) and writes, in the
current directory: frameset-frames.json (one row per run member: group, frame id,
measured FWHM, weight, included, the run's lnScale, the calibrated path with the
working-folder prefix remapped to the acceptance copy, the registration map file and
the per-channel global scale read back from the member's .athln sidecar),
frameset-maps/<frame>.json (each member's transform_json), and for every
<group-key> <every-nth> pair frameset-<group-key>-args.txt: `--warp` arguments for
every nth included member by measured FWHM (plus the broadest).
"""
import json
import os
import sqlite3
import struct
import sys


def athln_scales(path):
    """Per-channel global scale of an .athln sidecar (grid.rs layout: magic[8], version, width,
    height, scale, channels; per channel gw, gh, global_scale, location_ref, location_tgt,
    global_scale, then a and b as gw*gh f32 each)."""
    b = open(path, "rb").read()
    _ver, _w, _h, _sc, n = struct.unpack_from("<5I", b, 8)
    off, out = 28, []
    for _ in range(n):
        gw, gh = struct.unpack_from("<2I", b, off)
        off += 8
        out.append(struct.unpack_from("<d", b, off)[0])
        off += 32 + 8 * gw * gh
    return out


def main():
    db, set_id, run_id, old, new = sys.argv[1], int(sys.argv[2]), int(sys.argv[3]), sys.argv[4], sys.argv[5]
    picks = list(zip(sys.argv[6::2], (int(v) for v in sys.argv[7::2])))
    con = sqlite3.connect(db)
    summary = json.loads(con.execute("SELECT summary_json FROM stacking_runs WHERE id = ?", (run_id,)).fetchone()[0])
    os.makedirs("frameset-maps", exist_ok=True)
    rows = []
    for g in summary["groups"]:
        key = g["key"]
        for f in g["frames"]:
            fid = f["frameId"]
            art = lambda kind: con.execute(
                "SELECT path FROM stacking_artifacts WHERE frames_set_id = ? AND group_key = ? AND kind = ? AND frame_id = ?",
                (set_id, key, kind, fid)).fetchone()
            cal, ln = art("calibrated"), art("ln")
            tj = con.execute("SELECT transform_json FROM registration_results WHERE frames_set_id = ? AND frame_id = ?",
                             (set_id, fid)).fetchone()
            if not (cal and ln and tj and tj[0]):
                print(f"frame {fid}: no calibrated/ln/registration row, skipped", file=sys.stderr)
                continue
            mp = f"frameset-maps/{fid}.json"
            open(mp, "w").write(tj[0])
            rows.append(dict(group=key, frame=fid, fwhm=f["fwhmPx"], ecc=f["eccentricity"], weight=f["weight"],
                             included=f["included"], lnScale=f["lnScale"], cal=cal[0].replace(old, new), map=mp,
                             athln=athln_scales(ln[0].replace(old, new)), athlnPath=ln[0].replace(old, new)))
    json.dump(rows, open("frameset-frames.json", "w"), indent=0)
    for key, step in picks:
        r = sorted((x for x in rows if x["group"] == key and x["included"]), key=lambda x: x["fwhm"])
        sel = r[::step]
        if r and r[-1] not in sel:
            sel.append(r[-1])
        open(f"frameset-{key}-args.txt", "w").write(" ".join(f"--warp f{x['frame']}={x['cal']}@{x['map']}" for x in sel))
        print(key, len(sel), "members picked, FWHM", [round(x["fwhm"], 2) for x in sel])


if __name__ == "__main__":
    main()
