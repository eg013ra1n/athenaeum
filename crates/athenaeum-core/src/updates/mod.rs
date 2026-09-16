//! In-app updates — the host-independent half (spec
//! `docs/superpowers/specs/2026-09-16-in-app-updates-design.md` §2.2):
//! manifest fetch, channel choice, SemVer comparison, release notes and the
//! once-per-version "What's new". Download/verify/install live in the
//! desktop host (`tauri-plugin-updater`); this module never touches a file.

pub mod version;
