# Perseus package smoke test (per platform)

One pass per installer artifact of a tagged build (macOS arm64 + x64 DMG,
Windows setup.exe, Linux amd64 desktop deb, arm64 headless deb). Steps 1–8
apply to the desktop platforms; step 9 replaces them for headless nodes.

1. Install from the built artifact (dmg / setup.exe / deb).
2. Launch → tray icon appears as a hollow ring (needs setup); the menu status
   line reads "Setup required: no capture folders configured, not signed in".
3. Open Web UI → page loads at `http://127.0.0.1:8686`, Account card visible,
   agent banner yellow with the same needs.
4. Sign in (email → code from mail) → banner updates, tray icon still a ring
   (capture folders missing).
5. Add a capture folder in the web editor → within ~30 s the icon goes solid,
   status "Watching 1 folder(s)".
6. Copy a real FITS into the folder → icon shows syncing ("Syncing 1
   package(s)"), the primary receives the frame (check TransfersPanel on the
   primary), then back to "Watching…".
7. Toggle "Start at login" on. macOS: expect a one-time Automation permission
   prompt (System Events) on first toggle — grant it; the checkbox reflects
   the actual outcome, and the menu item is briefly disabled while the toggle
   is in flight. Re-login to the OS → Perseus auto-starts.
8. Quit from the tray → process exits cleanly (no `perseus` left in `pgrep`).
9. Headless (RPi / server): install the headless deb, set `web_bind` to a LAN
   address **and `web_token`** in the config (non-loopback refuses to start
   without a token), run `perseus run` (or the bundled systemd example unit
   from `/usr/share/doc/perseus/examples/`), open `http://<node-ip>:8686`
   from a desktop browser, enter the token when the page asks, then repeat
   steps 4–6 from the web page alone.

Windows-specific notes: the installer's finish-page "Launch Perseus (tray)"
runs the first instance elevated (installer token) — quit it and relaunch
from the Start Menu for a normal-privilege session before judging autostart
behavior. The "Start with Windows" installer checkbox writes the launch for
the account that ran the installer.

macOS-specific notes: the DMG is signed + notarized — no quarantine
workaround expected; if Gatekeeper complains, that IS a finding.
