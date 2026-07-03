# Athenaeum File Locations by Platform

App identifier: `com.vsharifov.athenaeum`

---

## macOS

| What | Path |
| ---- | ---- |
| **App bundle** | `/Applications/Athenaeum.app` |
| **App data** (SQLite DB, settings) | `~/Library/Application Support/com.vsharifov.athenaeum/` |
| **Logs** | `~/Library/Application Support/com.vsharifov.athenaeum/logs/` |
| **Cache** | `~/Library/Caches/com.vsharifov.athenaeum/` |
| **Config** | `~/Library/Application Support/com.vsharifov.athenaeum/` (same as app data on macOS) |

DB path: `~/Library/Application Support/com.vsharifov.athenaeum/athenaeum.db`

## Linux

| What | Path |
| ---- | ---- |
| **Binary** | `/usr/bin/athenaeum` (deb/rpm) or in AppImage |
| **Desktop file** | `/usr/share/applications/athenaeum.desktop` |
| **Icons** | `/usr/share/icons/hicolor/...` |
| **App data** (SQLite DB) | `~/.local/share/com.vsharifov.athenaeum/` |
| **Config** | `~/.config/com.vsharifov.athenaeum/` |
| **Cache** | `~/.cache/com.vsharifov.athenaeum/` |
| **Logs** | `~/.local/share/com.vsharifov.athenaeum/logs/` |

These follow the [XDG Base Directory Specification](https://specifications.freedesktop.org/basedir-spec/latest/). If `$XDG_DATA_HOME`, `$XDG_CONFIG_HOME`, or `$XDG_CACHE_HOME` are set, those override the defaults.

## Windows

| What | Path |
| ---- | ---- |
| **Installed app** | `C:\Program Files\Athenaeum\` (MSI) or `C:\Users\{user}\AppData\Local\Athenaeum\` (NSIS) |
| **App data** (SQLite DB) | `C:\Users\{user}\AppData\Roaming\com.vsharifov.athenaeum\` |
| **Local data / Cache** | `C:\Users\{user}\AppData\Local\com.vsharifov.athenaeum\` |
| **Config** | `C:\Users\{user}\AppData\Roaming\com.vsharifov.athenaeum\` (same as app data) |
| **Logs** | `C:\Users\{user}\AppData\Roaming\com.vsharifov.athenaeum\logs\` |

## Docker / Web

| What | Path |
| ---- | ---- |
| **App data** (SQLite DB) | `/data/` (or `$ATHENAEUM_DB_PATH`'s directory) |
| **Logs** | `/data/logs/` |

---

## Tauri 2.0 API Mappings

These paths come from Tauri's `path` module (accessible in both Rust and JS):

| Tauri API | macOS | Linux | Windows |
| --------- | ----- | ----- | ------- |
| `app_data_dir()` | `~/Library/Application Support/{id}` | `~/.local/share/{id}` | `AppData\Roaming\{id}` |
| `app_config_dir()` | same as app_data | `~/.config/{id}` | same as app_data |
| `app_cache_dir()` | `~/Library/Caches/{id}` | `~/.cache/{id}` | `AppData\Local\{id}` |
| `app_log_dir()` | `~/Library/Logs/{id}` | `~/.local/share/{id}/logs` | `AppData\Roaming\{id}\logs` |

Where `{id}` = `com.vsharifov.athenaeum` from `tauri.conf.json`.

**Note (as of the v0.2.3 logging overhaul):** Athenaeum no longer calls `app_log_dir()`. Logs are resolved by `athenaeum_core::logging::resolve_log_dir()` as `<app-data>/logs/` — i.e. `app_data_dir()` + `logs/` — which matches `app_log_dir()` only on Windows; on macOS and Linux the app's actual log directory is the "Logs" row in the platform tables above, not `app_log_dir()`. Inside `logs/`, files rotate daily as JSONL with a per-process filename prefix (`athenaeum-desktop.<date>.jsonl` / `athenaeum-web.<date>.jsonl`, 14-file cap) so desktop and web pointed at the same app-data dir never race. The legacy single-file `athenaeum.log` (and `athenaeum.log.1`) written by the pre-overhaul logger is deleted on first run of the new subscriber.
