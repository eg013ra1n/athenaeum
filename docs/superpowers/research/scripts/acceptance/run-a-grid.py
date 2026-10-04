#!/usr/bin/env python3
"""The canonical grid an acceptance run stacks a project onto, written to an
acceptance catalog COPY only (plan 6b-3 D21, spec §15.3).

    run-a-grid.py <copy.db> <project-id> --reference-uuid <uuid> [--apply]
    run-a-grid.py <copy.db> <project-id> --grid-json '<json>' [--apply]

--reference-uuid (run A): the grid of that frame's manifest WCS
(`project_frames_local.manifest_json` -> `meta.wcs`) — scale sqrt(|det CD|)·3600,
the frame's size (`meta.naxis1/2`), the sky position of its centre pixel
((W+1)/2, (H+1)/2) through the WCS with SIP, and the rotation the grid's own
TAN reads back (`field_rotation_deg`: atan2(-CD1_2, CD2_2) in [0, 360)). The
grid's CD is sky-standard (det < 0), so a det > 0 reference comes out mirrored
in x with north kept (6b-2 open item "the grid's orientation").
--grid-json (run B): any grid, e.g. the `proposal` of `get_collab_grid_proposal`.

Prints the normalized grid. --apply writes it to
`collab_projects.canonical_grid_json`. Refuses the live catalogs: a real
project's grid is its coordinator's, set on the hub, never here.
"""
import json
import math
import os
import sqlite3
import sys
import urllib.parse

LIVE = [
    os.path.realpath(os.path.expanduser(p))
    for p in (
        "~/Library/Application Support/com.vsharifov.athenaeum.dev/athenaeum.db",
        "~/Library/Application Support/com.vsharifov.athenaeum/athenaeum.db",
    )
]


def sip(table, u, v):
    return sum(c * (u ** p) * (v ** q) for p, row in enumerate(table) for q, c in enumerate(row) if c)


def pixel_to_sky(wcs, x1, y1):
    """1-based pixel -> (ra, dec) degrees through TAN with the forward SIP."""
    u, v = x1 - wcs["crpix1"], y1 - wcs["crpix2"]
    s = wcs.get("sip")
    if s:
        du, dv = sip(s["a"], u, v), sip(s["b"], u, v)
        u, v = u + du, v + dv
    cd = wcs["cd"]  # [CD1_1, CD1_2, CD2_1, CD2_2]
    xi = math.radians(cd[0] * u + cd[1] * v)
    eta = math.radians(cd[2] * u + cd[3] * v)
    ra0, dec0 = math.radians(wcs["crval1"]), math.radians(wcs["crval2"])
    den = math.cos(dec0) - eta * math.sin(dec0)
    ra = ra0 + math.atan2(xi, den)
    dec = math.atan2(math.sin(dec0) + eta * math.cos(dec0), math.hypot(xi, den))
    return math.degrees(ra) % 360.0, math.degrees(dec)


def normalized(g):
    rot = math.fmod(g["rotationDeg"], 360.0)
    if rot <= -180.0:
        rot += 360.0
    elif rot > 180.0:
        rot -= 360.0
    # `+ 0.0` turns -0.0 into 0.0 on every float, as `CanonicalGrid::normalized`
    # does (review M9).
    return {
        "crval1": (g["crval1"] % 360.0) + 0.0,
        "crval2": g["crval2"] + 0.0,
        "heightPx": int(g["heightPx"]),
        "rotationDeg": rot + 0.0,
        "scaleArcsec": g["scaleArcsec"] + 0.0,
        "widthPx": int(g["widthPx"]),
    }


def grid_of_reference(db, pid, uuid):
    row = db.execute(
        "SELECT manifest_json FROM project_frames_local WHERE project_id = ? AND frame_uuid = ?",
        (pid, uuid),
    ).fetchone()
    if not row:
        sys.exit(f"no frame {uuid} in project {pid}")
    meta = json.loads(row[0]).get("meta") or {}
    wcs = meta.get("wcs")
    if not wcs:
        sys.exit(f"frame {uuid} has no manifest WCS: pick a reference that has one")
    w, h = int(meta["naxis1"]), int(meta["naxis2"])
    cd = wcs["cd"]
    ra, dec = pixel_to_sky(wcs, (w + 1) / 2.0, (h + 1) / 2.0)
    rot = math.degrees(math.atan2(-cd[1], cd[3])) % 360.0
    scale = math.sqrt(abs(cd[0] * cd[3] - cd[1] * cd[2])) * 3600.0
    # Review I4: the grid's pixels are square and unskewed; the reference's
    # are only to the extent its CD is. Print how far, so run A's report says
    # how much of a row difference geometry could explain.
    col1, col2 = math.hypot(cd[0], cd[2]), math.hypot(cd[1], cd[3])
    aspect = col1 / col2 if col2 else float("nan")
    skew = abs(cd[0] * cd[1] + cd[2] * cd[3]) / (col1 * col2) if col1 and col2 else float("nan")
    print(
        f"reference {uuid}: scale {scale:.6f}\"/px, rotation {rot:.4f} deg, "
        f"pixel aspect {aspect:.6f}, skew {skew:.2e}, det CD {'>' if cd[0] * cd[3] - cd[1] * cd[2] > 0 else '<'} 0",
        file=sys.stderr,
    )
    return {"scaleArcsec": scale, "widthPx": w, "heightPx": h, "crval1": ra, "crval2": dec, "rotationDeg": rot}


def main():
    args = sys.argv[1:]
    if len(args) < 4:
        sys.exit(__doc__)
    db_path, pid, mode, value = args[:4]
    apply = "--apply" in args[4:]
    if os.path.realpath(db_path) in LIVE:
        sys.exit("refusing: that is a live catalog — work on an acceptance copy")
    try:
        # mode=rw: a typo in the path must never create an empty database.
        db = sqlite3.connect(f"file:{urllib.parse.quote(os.path.abspath(db_path))}?mode=rw", uri=True)
    except sqlite3.Error as e:
        sys.exit(f"cannot open {db_path} read-write: {e}")
    if mode == "--reference-uuid":
        grid = grid_of_reference(db, pid, value)
    elif mode == "--grid-json":
        grid = json.loads(value)
        if isinstance(grid, dict) and "proposal" in grid:
            # The whole `get_collab_grid_proposal` answer is accepted; a null proposal is the
            # command saying there is nothing to propose (no solved frame to build one from).
            if grid["proposal"] is None:
                sys.exit("the grid proposal is null: the project has no proposal to apply")
            grid = grid["proposal"]
        if not isinstance(grid, dict) or not all(
            k in grid for k in ("crval1", "crval2", "heightPx", "rotationDeg", "scaleArcsec", "widthPx")
        ):
            sys.exit(f"--grid-json is not a grid (crval1, crval2, heightPx, rotationDeg, scaleArcsec, widthPx): {value}")
    else:
        sys.exit(__doc__)
    grid = normalized(grid)
    ok = (
        all(math.isfinite(grid[k]) for k in ("crval1", "crval2", "rotationDeg", "scaleArcsec"))
        and 0.0 <= grid["crval1"] < 360.0
        and 0 < grid["scaleArcsec"] <= 100
        and 64 <= grid["widthPx"] <= 65536
        and 64 <= grid["heightPx"] <= 65536
        and -90 <= grid["crval2"] <= 90
    )
    if not ok:
        sys.exit(f"grid outside the hub's bounds: {grid}")
    text = json.dumps(grid, sort_keys=True, separators=(",", ":"))
    print(text)
    if apply:
        n = db.execute(
            "UPDATE collab_projects SET canonical_grid_json = ? WHERE project_id = ?", (text, pid)
        ).rowcount
        db.commit()
        if n != 1:
            sys.exit(f"project {pid} not in {db_path}")
        print(f"stored on project {pid} in {db_path}")


if __name__ == "__main__":
    main()
