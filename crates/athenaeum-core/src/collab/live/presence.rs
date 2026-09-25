//! The presence book (spec §4.2): the app's current view of who else is
//! online and serving, per project. `hello.projects[*].presence` seeds it;
//! `presence` events patch or rebuild it. A device is a "candidate" — a peer
//! worth dialing for a frame — only when it is both connected and serving;
//! callers that just need "is this device online at all" use
//! [`PresenceBook::is_connected`] instead.

use std::collections::HashMap;
use std::time::Duration;

use super::wire::{PresenceEntry, PresenceEvent};

/// How often the app sends its presence beat (spec §4.2).
pub const BEAT_INTERVAL: Duration = Duration::from_secs(15);
/// Hub-owned: a session with no beat this long is dropped (spec §4.2). The
/// app shows this only; it does not enforce it locally.
pub const HUB_SILENCE: Duration = Duration::from_secs(40);
/// Hub-owned: how long a closed stream's session survives before the hub
/// publishes it offline (spec §4.2) — a same-device reconnect inside this
/// window replaces the session with no presence flap.
pub const HUB_GRACE: Duration = Duration::from_secs(10);
/// A wall-clock step this much larger than the matching monotonic step means
/// the machine slept (plan P28).
pub const WAKE_JUMP: Duration = Duration::from_secs(30);

/// One device's presence as the app currently knows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerPresence {
    pub connected: bool,
    pub serving: bool,
    pub relay_url: Option<String>,
}

impl PeerPresence {
    /// Worth dialing for a frame: online AND currently serving.
    fn is_candidate(&self) -> bool {
        self.connected && self.serving
    }
}

/// project id → device → [`PeerPresence`].
#[derive(Debug, Default, Clone)]
pub struct PresenceBook {
    projects: HashMap<String, HashMap<String, PeerPresence>>,
}

impl PresenceBook {
    /// `hello.projects[*].presence`: replaces that project's presence set
    /// with EXACTLY these entries (every one implicitly connected — the hub
    /// lists only currently connected devices, `[]` during its warm-up).
    pub fn apply_hello(&mut self, project_id: &str, entries: &[PresenceEntry]) {
        let map = entries
            .iter()
            .map(|e| {
                (
                    e.device.clone(),
                    PeerPresence {
                        connected: true,
                        serving: e.serving,
                        relay_url: e.relay_url.clone(),
                    },
                )
            })
            .collect();
        self.projects.insert(project_id.to_string(), map);
    }

    /// A `presence` event. `replace: true` rebuilds the project's presence
    /// set from `changes` alone (the hub's post-warm-up snapshot, once);
    /// otherwise each change patches (or adds) that one device, leaving
    /// every other device's entry untouched. Returns the devices whose
    /// candidacy (connected && serving) flipped, sorted — a device whose
    /// relay alone moved, or one dropped by a fresh warm-up while it was
    /// never a candidate, is not reported.
    pub fn apply_event(&mut self, ev: &PresenceEvent) -> Vec<String> {
        let map = self.projects.entry(ev.project_id.clone()).or_default();
        let dropped = if ev.replace {
            Some(std::mem::take(map))
        } else {
            None
        };
        let mut changed = Vec::new();
        for c in &ev.changes {
            let before = match &dropped {
                Some(prev) => prev.get(&c.device).is_some_and(PeerPresence::is_candidate),
                None => map.get(&c.device).is_some_and(PeerPresence::is_candidate),
            };
            let peer = PeerPresence {
                connected: c.connected,
                serving: c.serving,
                relay_url: c.relay_url.clone(),
            };
            let after = peer.is_candidate();
            map.insert(c.device.clone(), peer);
            if before != after {
                changed.push(c.device.clone());
            }
        }
        // A replace drops every device the `changes` list didn't mention; a
        // dropped device that was a candidate loses that status too.
        if let Some(prev) = dropped {
            for (device, peer) in prev {
                if !map.contains_key(&device) && peer.is_candidate() {
                    changed.push(device);
                }
            }
        }
        changed.sort();
        changed
    }

    /// Drop everything known about a project (e.g. `account: left`).
    pub fn forget_project(&mut self, project_id: &str) {
        self.projects.remove(project_id);
    }

    /// Online and currently serving — worth dialing for a frame.
    pub fn is_online_serving(&self, project_id: &str, device: &str) -> bool {
        self.projects
            .get(project_id)
            .and_then(|m| m.get(device))
            .is_some_and(PeerPresence::is_candidate)
    }

    pub fn relay_url(&self, project_id: &str, device: &str) -> Option<&str> {
        self.projects
            .get(project_id)?
            .get(device)?
            .relay_url
            .as_deref()
    }

    pub fn is_connected(&self, project_id: &str, device: &str) -> bool {
        self.projects
            .get(project_id)
            .and_then(|m| m.get(device))
            .is_some_and(|p| p.connected)
    }
}

/// A wall-clock step larger than the monotonic step by more than
/// [`WAKE_JUMP`] means the machine slept (plan P28: tauri has no desktop
/// sleep event, so the beat loop detects the wake and reconnects at once).
pub fn woke_from_sleep(wall_elapsed: Duration, mono_elapsed: Duration) -> bool {
    wall_elapsed.saturating_sub(mono_elapsed) > WAKE_JUMP
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collab::live::wire::{PresenceChange, PresenceEntry, PresenceEvent};

    #[test]
    fn hello_replaces_and_events_patch_or_replace() {
        let mut b = PresenceBook::default();
        b.apply_hello(
            "p1",
            &[PresenceEntry {
                device: "A".into(),
                serving: true,
                relay_url: Some("https://r1".into()),
            }],
        );
        assert!(b.is_online_serving("p1", "A"));
        let changed = b.apply_event(&PresenceEvent {
            project_id: "p1".into(),
            replace: false,
            changes: vec![
                PresenceChange {
                    device: "B".into(),
                    connected: true,
                    serving: false,
                    relay_url: None,
                },
                PresenceChange {
                    device: "A".into(),
                    connected: true,
                    serving: true,
                    relay_url: Some("https://r2".into()),
                },
            ],
        });
        // A stays a candidate (only its relay moved); B is connected but not serving
        assert!(changed.is_empty());
        assert_eq!(b.relay_url("p1", "A"), Some("https://r2"));
        assert!(b.is_connected("p1", "B") && !b.is_online_serving("p1", "B"));
        let changed = b.apply_event(&PresenceEvent {
            project_id: "p1".into(),
            replace: true,
            changes: vec![PresenceChange {
                device: "B".into(),
                connected: true,
                serving: true,
                relay_url: None,
            }],
        });
        assert_eq!(changed, vec!["A".to_string(), "B".to_string()]); // A dropped by replace, B now serving
        assert!(!b.is_connected("p1", "A"));
        assert!(b.is_online_serving("p1", "B"));
    }

    #[test]
    fn a_wall_clock_jump_beyond_the_beat_means_the_machine_slept() {
        assert!(!woke_from_sleep(
            Duration::from_secs(16),
            Duration::from_secs(15)
        ));
        assert!(woke_from_sleep(
            Duration::from_secs(120),
            Duration::from_secs(15)
        ));
    }
}
