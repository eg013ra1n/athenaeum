#!/usr/bin/env python3
"""The set_stacking_config body for the Horse C11 acceptance copy (spec §11.4).

    horse-c11-config.py <run-8.json> <acc-dir/athenaeum.db> [--fixed-1024]

Prints {"setId": 182, "config": <run-8's config>, "excludedFrameIds": [<set 182 lights whose
FILTER is R or B>]}. <run-8.json> is the run summary (its "config" object) or a bare config.
--fixed-1024 sets normalization.local.scaleMode = "fixed", scale = 1024 (the optional fixed run).
Reads only the given catalog COPY, read-only.
"""
import json
import sqlite3
import sys

SET_ID = 182


def main():
    args = sys.argv[1:]
    fixed = "--fixed-1024" in args
    args = [a for a in args if a != "--fixed-1024"]
    if len(args) != 2:
        sys.exit(__doc__)
    doc = json.load(open(args[0]))
    config = doc.get("config", doc) if isinstance(doc, dict) else doc
    if not isinstance(config, dict) or "normalization" not in config:
        sys.exit(f"{args[0]}: no stacking config (expected a 'config' object with 'normalization')")
    if fixed:
        local = config["normalization"].setdefault("local", {})
        local["scaleMode"] = "fixed"
        local["scale"] = 1024
    conn = sqlite3.connect(f"file:{args[1]}?mode=ro", uri=True)
    rows = conn.execute(
        """SELECT DISTINCT f.id FROM frames f
           JOIN session_members sm ON sm.frame_id = f.id
           JOIN sessions s ON s.id = sm.session_id
           JOIN imaging_nights n ON n.id = s.imaging_night_id
           WHERE n.frames_set_id = ? AND upper(f.imagetyp) = 'LIGHT'
             AND upper(trim(f.filter)) IN ('R', 'B')
           ORDER BY f.id""",
        (SET_ID,),
    ).fetchall()
    conn.close()
    print(json.dumps({"setId": SET_ID, "config": config, "excludedFrameIds": [r[0] for r in rows]}))


if __name__ == "__main__":
    main()
