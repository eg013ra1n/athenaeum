# Open items — in-app updates (to be merged into `open-items.md`)

`docs/superpowers/open-items.md` was modified in the working tree by another
session while this task ran, so this subsection is written to its own file
instead of being spliced in directly. It uses the same bullet style as the
rest of that document — merge it in as a new `##` section once the other
edit lands, and delete this file.

## In-app updates: two-tag acceptance and a deferred platform

Spec `docs/superpowers/specs/2026-09-16-in-app-updates-design.md` §7.3 has
the acceptance procedure in full; automated coverage (core, the web routes,
the comparator parity test, the CI manifest/verify scripts) is green, but
the feature cannot be exercised end to end until it ships in a real tag —
the first build carrying the updater cannot itself be received through it.

**Owner smokes owed, both on `v0.6.5-beta.1` and `v0.6.5-beta.2`:**

- `v0.6.5-beta.1` installed by hand on macOS arm64, macOS x64, Windows,
  Linux AppImage and Docker; pipeline green including the new
  `verify:release` manifest/signature checks; `updates/v0.6.5-beta.1.json`
  and `latest-beta.json` present on `artfrom.space`; the adoption dashboard
  shows pings from the new `/updates/latest*.json` paths (astronet's parser
  switched over, see below).
- `v0.6.5-beta.2` (any minimal change) on every OS: dialog → install →
  restart → *What's new in v0.6.5-beta.2*; `spctl --assess --type execute`
  passes on the UPDATED macOS bundle (proves the plugin's replacement is
  still a validly signed/notarized `.app`, not just the original install);
  the Docker container still on beta.1 shows *available* with
  `docker pull vsharifov/athenaeum:0.6.5-beta.2`.

Write the run up as `docs/superpowers/research/<date>-updater-acceptance-run.md`
per the spec, and delete this bullet once it lands green.

**`.deb` installs — deferred, not a bug.** `platform_supported` refuses
`deb`/`rpm` by rule (`crates/athenaeum-core/src/updates/manifest.rs`): a
package-manager install falls back to nothing rather than being handed the
AppImage entry and choking on `dpkg -i` after a 100 MB download. The
updater plugin itself supports installing over a `.deb` (`dpkg -i` under the
hood), so a `linux-x86_64-deb` manifest key plus a `.deb` build/sign leg is
a real, scoped follow-up — not attempted in this cycle (spec §8 lists it
under "Out of scope"). Do not re-flag as a defect; it is a deliberate gap
pending its own task.
