---
name: bug-triage
description: Use when the user says there are new bug reports in the Bug_reports/Incoming folder, shares a user's JSONL log file to investigate, or asks to triage, process, or analyze a bug report from a user.
---

# Bug Report Triage

Turn a user-submitted JSONL log + the owner's comment into a root-cause analysis
report in the Processed folder, ready for planning a fix.

## Folders

- **Inbox**: `/Volumes/BigMac/Users/astrobureau/Documents/Projects/Bug_reports/Incoming`
- **Output**: `/Volumes/BigMac/Users/astrobureau/Documents/Projects/Bug_reports/Processed`
- **Index**: `/Volumes/BigMac/Users/astrobureau/Documents/Projects/Bug_reports/INDEX.md`

## Input format

Each report is one JSONL file — the app's `tracing` log
(`athenaeum-desktop.*` / `athenaeum-web.*`), one JSON object per line:

```json
{"timestamp":"…Z","level":"ERROR","fields":{"error":"…"},"target":"athenaeum_lib::commands::scan_roots","span":{"name":"set_sync_incoming_dir"},"spans":[…]}
```

- `target` = Rust module path; `span.name` = the Tauri command / Axum route.
- `fields.message` is a **short stable phrase** (project logging convention) —
  `rg "exact phrase" crates/` finds the emit site directly.
- `"message":"close"` with `time.busy`/`time.idle` = command-boundary span
  timing, not an event. Command failures appear as `error`-field events inside
  the span.

The owner gives a comment per file describing the complained-about problem.
**No comment for a file → ask before analyzing** — the comment anchors the
investigation; a log alone can contain several unrelated anomalies.

## Workflow

Reports are written in **English**; the owner's comment stays verbatim in its
original language.

1. **Inventory** the Inbox; skip files already analyzed. A file counts as
   analyzed when a JSONL with the same name AND same content (compare
   `xxhsum`/`shasum` if in doubt) exists in any Processed subfolder — name
   alone is not the key, different users produce identically-named files.
   Confirm each new file has a comment.
2. **Extract signal with grep/jq — never read the whole JSONL into context**:
   - all ERROR lines; WARN lines deduped by message with counts;
   - lines in the time window / module the comment points at
     (filter by `target`, `span.name`, timestamp prefix).
3. **Map to code**: `rg` the stable message phrase in `crates/` (and
   submodules), read the emit site and trace the real code path. Root-cause
   from code, not from the log text alone. For several reports, dispatch one
   investigation subagent per report in parallel. **Subagents only
   investigate and return the filled template as text** — the top-level
   session writes all files, moves JSONLs, and updates the index (the harness
   blocks report-file writes from subagents).
4. **Write the report**: create
   `Processed/<YYYY-MM-DD>_<symptom-slug>/report.md` (date = log date; slug =
   2–4 words naming the SYMPTOM the user experienced, not the cause — e.g.
   `star-metrics-missing`, not `analysis-read-failure`).
5. **Move the source JSONL** from Incoming into that same subfolder — the
   Inbox stays a clean queue, and report + evidence live together. Any
   command lines quoted in the report must reference the post-move path.
6. **Update INDEX.md** (create if missing), newest row first, MD060 style
   (spaces around dashes in the separator row):

   ```markdown
   | Date | Folder | Symptom | Root cause (short) | Status |
   | ---- | ------ | ------- | ------------------ | ------ |
   ```

   Status is one of `analyzed → planned → fixed → released`.
7. **Hand off**: summarize findings to the owner and propose the next step —
   superpowers:brainstorming when the fix design is unclear,
   superpowers:writing-plans when it's clear; implementation later via agents
   per the owner's subagent rules (rust-engineer / frontend-dev, opus).

## Report template

```markdown
# <one-line symptom>

- **Source**: <jsonl filename> (desktop|web, log date <YYYY-MM-DD>)
- **User comment**: <verbatim>
- **App version**: <if determinable, else "unknown">
- **Severity**: <see rubric below>
- **Status**: analyzed

## Symptom
What the user experienced, one short paragraph.

## Evidence
Key log lines (quoted, trimmed) with counts and the incident time window.

## Root cause
The mechanism, with `crates/...:line` references. If not fully pinned:
best hypothesis, confidence, and what evidence would confirm it.

## Affected code
Files/functions involved — including BOTH backends when a command is involved
(commands/<domain>.rs + routes/<domain>.rs) and the core module.

## Fix directions
1–3 candidate approaches, one recommended, with scope
(core / tauri+web mirror / frontend).

## Open questions / needed from user
Missing evidence, repro steps, or a debug-level log to request.
```

**Severity rubric**:

- `critical` — data loss, corruption, crash, or catalog integrity broken
- `major` — a core workflow degraded, blocked, or producing wrong results
- `minor` — workaround exists; annoyance, noise, or perf degradation
- `cosmetic` — UI/wording only, no functional impact

## Gotchas

- **WARN floods**: dedupe first — 400× the same message is one issue with a
  hot loop, not 400 issues. The repeat count itself is evidence.
- **The lone ERROR may be unrelated** to the user's complaint. Anchor on the
  comment and timestamps, not on level alone. List secondary anomalies in the
  report, but keep the analysis focused on the complained-about problem.
- **Default level is `info`** — debug/trace evidence is usually absent from
  user logs. If evidence is insufficient, say exactly what's missing and give
  the user-facing instruction to include in the reply. Per-module overrides in
  Settings → Logging exist ONLY for scanner / solver / calibration /
  archive+file_op — for those, raise the module to `debug`; for anything else
  the instruction is: raise the GLOBAL level to `debug`, reproduce, re-send.
- **App version is not logged.** Ask the owner, or pin a minimum version by
  matching message phrases against git history (a phrase introduced in a
  known release bounds the version).
- **Framework noise**: `NewEvents emitted without explicit
  RedrawEventsCleared` / `RedrawEventsCleared emitted without explicit
  MainEventsCleared` (tao/winit event loop) — ignore.
- **Same-named files collide**: log filenames are date-based
  (`athenaeum-desktop.<date>.jsonl`), so two users can send identical names —
  the per-bug subfolder is what disambiguates; never leave analyses as loose
  files in Processed.

## Quick jq/grep recipes

```bash
F=Incoming/<file>.jsonl
grep '"level":"ERROR"' $F                                      # all errors
grep '"level":"WARN"' $F | python3 -c "import sys,json;from collections import Counter;c=Counter(json.loads(l)['fields'].get('message','') for l in sys.stdin);[print(n,m) for m,n in c.most_common(15)]"
grep '"target":"athenaeum_core::scanner' $F | head -50          # by module
grep '"timestamp":"2026-07-18T06:4' $F                          # time window
python3 -c "import json,sys;[print(json.dumps(json.loads(l),indent=1)) for l in open('$F') if 'phrase' in l]"  # pretty-print matches
```
