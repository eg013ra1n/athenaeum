//! The client's holder map (spec §6.1, I4/I5): per project, which device
//! claims which frame (`frameSeq`) at which content version, plus each
//! device's display name and last known relay. Pure data and derivations —
//! persistence and the hub calls live in `api::collab_live::holdings`.
//!
//! Providers are DERIVED on every read (§6.1): a device is a candidate for a
//! frame only when its claimed version equals the manifest's current one, it
//! is connected and serving the project, it is a member, and it is not me.
//! Nothing here is cleared by a version event, so the order in which
//! `project` and `holders` events arrive never matters.

use std::collections::{HashMap, HashSet};

use crate::collab::live::presence::PresenceBook;
use crate::collab::live::wire::{expand_runs, HolderDeltaWire, HoldersSnapshotWire};
use crate::db::collab_live::HolderDeviceRow;

/// A holder device's name and last known relay (from the snapshot; a device
/// first seen in a delta has an empty name until the next snapshot).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceInfo {
    pub display_name: String,
    pub relay_url: Option<String>,
}

/// One project's holder map. `claims`: device → frameSeq → contentVersion.
/// Every known device has a `claims` entry, possibly empty — so a map built
/// from a snapshot, from persisted rows or by deltas compares equal when it
/// describes the same holdings.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct ProjectHolders {
    pub devices: HashMap<String, DeviceInfo>,
    pub claims: HashMap<String, HashMap<i32, i32>>,
}

impl ProjectHolders {
    /// A whole map from `GET …/holders/snapshot` (run-length claims expanded).
    pub fn from_snapshot(s: &HoldersSnapshotWire) -> Self {
        let mut h = ProjectHolders::default();
        for d in &s.devices {
            h.devices.insert(
                d.device.clone(),
                DeviceInfo {
                    display_name: d.display_name.clone(),
                    relay_url: d.relay_url.clone(),
                },
            );
            let claims = h.claims.entry(d.device.clone()).or_default();
            for (seq, cv) in expand_runs(&d.claims) {
                claims.insert(seq, cv);
            }
        }
        h
    }

    /// A map from its persisted rows (`db::collab_live::load_holders`) —
    /// mirrors [`Self::from_snapshot`]: every device gets a claims entry,
    /// even with no claims (T6 ruling), and a claim row whose device has no
    /// device row gets a placeholder device.
    pub fn from_rows(devices: &[HolderDeviceRow], claims: &[(String, i32, i32)]) -> Self {
        let mut h = ProjectHolders::default();
        for d in devices {
            h.devices.insert(
                d.device.clone(),
                DeviceInfo {
                    display_name: d.display_name.clone(),
                    relay_url: d.relay_url.clone(),
                },
            );
            h.claims.entry(d.device.clone()).or_default();
        }
        for (device, seq, cv) in claims {
            h.devices
                .entry(device.clone())
                .or_insert_with(placeholder_device);
            h.claims
                .entry(device.clone())
                .or_default()
                .insert(*seq, *cv);
        }
        h
    }

    /// One device's delta (`add` = `(frameSeq, contentVersion)`, `rm` =
    /// `frameSeq`). A claim on a `frameSeq` the manifest does not know yet is
    /// kept (hub § "holders").
    pub fn apply_delta(&mut self, d: &HolderDeltaWire) {
        self.devices
            .entry(d.device.clone())
            .or_insert_with(placeholder_device);
        let claims = self.claims.entry(d.device.clone()).or_default();
        for (seq, cv) in &d.add {
            claims.insert(*seq, *cv);
        }
        for seq in &d.rm {
            claims.remove(seq);
        }
    }

    /// Every device claiming exactly `(frame_seq, content_version)`, sorted.
    pub fn claimants(&self, frame_seq: i32, content_version: i32) -> Vec<&str> {
        let mut out: Vec<&str> = self
            .claims
            .iter()
            .filter(|(_, c)| c.get(&frame_seq) == Some(&content_version))
            .map(|(d, _)| d.as_str())
            .collect();
        out.sort_unstable();
        out
    }

    /// The device rows to persist, sorted by device.
    pub fn device_rows(&self) -> Vec<HolderDeviceRow> {
        let mut out: Vec<HolderDeviceRow> = self
            .devices
            .iter()
            .map(|(device, info)| HolderDeviceRow {
                device: device.clone(),
                display_name: info.display_name.clone(),
                relay_url: info.relay_url.clone(),
            })
            .collect();
        out.sort_by(|a, b| a.device.cmp(&b.device));
        out
    }

    /// The claim rows to persist, `(device, frameSeq, contentVersion)`,
    /// sorted by device then frame.
    pub fn claim_rows(&self) -> Vec<(String, i32, i32)> {
        let mut out: Vec<(String, i32, i32)> = self
            .claims
            .iter()
            .flat_map(|(device, c)| c.iter().map(move |(seq, cv)| (device.clone(), *seq, *cv)))
            .collect();
        out.sort();
        out
    }
}

fn placeholder_device() -> DeviceInfo {
    DeviceInfo {
        display_name: String::new(),
        relay_url: None,
    }
}

/// A device worth dialing for a frame, with the relay to reach it through.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Provider {
    pub device: String,
    pub relay_url: Option<String>,
}

/// Other holders of a frame's current version: how many are online and
/// serving right now, of how many in total (offline included).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Redundancy {
    pub online: usize,
    pub total: usize,
}

/// The frame a derivation is about: its project, its hub ordinal, the
/// manifest's CURRENT content version, and the publisher's devices (for the
/// "waiting for the publisher" state).
pub struct FrameRef<'a> {
    pub project_id: &'a str,
    pub frame_seq: i32,
    pub content_version: i32,
    pub publisher_devices: &'a HashSet<String>,
}

/// Other members' devices claiming the frame's current version (never me).
fn other_holders<'h>(
    h: &'h ProjectHolders,
    members: &HashSet<String>,
    me: &str,
    f: &FrameRef<'_>,
) -> Vec<&'h str> {
    h.claimants(f.frame_seq, f.content_version)
        .into_iter()
        .filter(|d| *d != me && members.contains(*d))
        .collect()
}

/// A frame's providers (spec I4 + I5): claims the CURRENT version, is
/// connected AND serving in this project, is a member, and is not me. The
/// relay is the live presence relay, else the snapshot's last known one.
/// Sorted by device. Never cached: availability is derived (§6.1).
pub fn providers(
    h: &ProjectHolders,
    presence: &PresenceBook,
    members: &HashSet<String>,
    me: &str,
    f: &FrameRef<'_>,
) -> Vec<Provider> {
    other_holders(h, members, me, f)
        .into_iter()
        .filter(|d| presence.is_online_serving(f.project_id, d))
        .map(|d| Provider {
            device: d.to_string(),
            relay_url: presence
                .relay_url(f.project_id, d)
                .map(str::to_string)
                .or_else(|| h.devices.get(d).and_then(|i| i.relay_url.clone())),
        })
        .collect()
}

/// How many OTHER member devices hold the frame's current version, and how
/// many of those are online and serving now.
pub fn redundancy(
    h: &ProjectHolders,
    presence: &PresenceBook,
    members: &HashSet<String>,
    me: &str,
    f: &FrameRef<'_>,
) -> Redundancy {
    let others = other_holders(h, members, me, f);
    Redundancy {
        online: others
            .iter()
            .filter(|d| presence.is_online_serving(f.project_id, d))
            .count(),
        total: others.len(),
    }
}

/// L7 "vN waiting for the publisher": the current version is held (by
/// another member device) ONLY by the publisher's own devices, and none of
/// them is online and serving.
pub fn waiting_for_publisher(
    h: &ProjectHolders,
    presence: &PresenceBook,
    members: &HashSet<String>,
    me: &str,
    f: &FrameRef<'_>,
) -> bool {
    let holders = other_holders(h, members, me, f);
    !holders.is_empty()
        && holders.iter().all(|d| f.publisher_devices.contains(*d))
        && holders
            .iter()
            .all(|d| !presence.is_online_serving(f.project_id, d))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collab::live::presence::PresenceBook;
    use crate::collab::live::wire::*;

    fn snap() -> HoldersSnapshotWire {
        HoldersSnapshotWire {
            epoch: "e".into(),
            holder_seq: 5,
            version: 9,
            frames: vec![SnapshotFrameWire {
                seq: 1,
                uuid: "u1".into(),
                content_version: 2,
            }],
            devices: vec![
                SnapshotDeviceWire {
                    device: "PUB".into(),
                    display_name: "Pub".into(),
                    relay_url: Some("https://r0".into()),
                    claims: vec![[1, 1, 2]],
                },
                SnapshotDeviceWire {
                    device: "OLD".into(),
                    display_name: "Old".into(),
                    relay_url: None,
                    claims: vec![[1, 1, 1]],
                },
                SnapshotDeviceWire {
                    device: "ME".into(),
                    display_name: "Me".into(),
                    relay_url: None,
                    claims: vec![[1, 1, 2]],
                },
                SnapshotDeviceWire {
                    device: "OFF".into(),
                    display_name: "Off".into(),
                    relay_url: None,
                    claims: vec![[1, 1, 2]],
                },
            ],
        }
    }

    fn members() -> HashSet<String> {
        ["PUB", "OLD", "ME", "OFF"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    }

    fn presence() -> PresenceBook {
        let mut p = PresenceBook::default();
        p.apply_hello(
            "p1",
            &[
                PresenceEntry {
                    device: "PUB".into(),
                    serving: true,
                    relay_url: Some("https://r1".into()),
                },
                PresenceEntry {
                    device: "OLD".into(),
                    serving: true,
                    relay_url: None,
                },
                PresenceEntry {
                    device: "ME".into(),
                    serving: true,
                    relay_url: None,
                },
            ],
        );
        p
    }

    #[test]
    fn providers_are_derived_from_version_presence_membership_and_not_me() {
        let h = ProjectHolders::from_snapshot(&snap());
        let pubs: HashSet<String> = ["PUB".to_string()].into();
        let f = FrameRef {
            project_id: "p1",
            frame_seq: 1,
            content_version: 2,
            publisher_devices: &pubs,
        };
        let got = providers(&h, &presence(), &members(), "ME", &f);
        // OLD claims v1 (superseded), OFF is offline, ME is me
        assert_eq!(
            got,
            vec![Provider {
                device: "PUB".into(),
                relay_url: Some("https://r1".into())
            }]
        );
        assert_eq!(
            redundancy(&h, &presence(), &members(), "ME", &f),
            Redundancy {
                online: 1,
                total: 2
            }
        );
        // not a member any more → never a provider
        let only_me: HashSet<String> = ["ME".to_string()].into();
        assert!(providers(&h, &presence(), &only_me, "ME", &f).is_empty());
    }

    #[test]
    fn a_provider_without_a_presence_relay_falls_back_to_the_snapshot_relay() {
        let h = ProjectHolders::from_snapshot(&snap());
        let pubs: HashSet<String> = HashSet::new();
        let f = FrameRef {
            project_id: "p1",
            frame_seq: 1,
            content_version: 2,
            publisher_devices: &pubs,
        };
        let mut p = PresenceBook::default();
        p.apply_hello(
            "p1",
            &[PresenceEntry {
                device: "PUB".into(),
                serving: true,
                relay_url: None,
            }],
        );
        assert_eq!(
            providers(&h, &p, &members(), "ME", &f),
            vec![Provider {
                device: "PUB".into(),
                relay_url: Some("https://r0".into())
            }]
        );
    }

    #[test]
    fn deltas_apply_in_any_order_relative_to_versions() {
        let mut h = ProjectHolders::from_snapshot(&snap());
        h.apply_delta(&HolderDeltaWire {
            device: "OLD".into(),
            add: vec![(1, 3)],
            rm: vec![],
        });
        h.apply_delta(&HolderDeltaWire {
            device: "PUB".into(),
            add: vec![],
            rm: vec![1],
        });
        assert_eq!(h.claimants(1, 3), vec!["OLD"]);
        assert_eq!(h.claimants(1, 2), vec!["ME", "OFF"]);
        // a claim on a frameSeq the manifest does not know yet is kept
        h.apply_delta(&HolderDeltaWire {
            device: "NEW".into(),
            add: vec![(99, 1)],
            rm: vec![],
        });
        assert_eq!(h.claimants(99, 1), vec!["NEW"]);
        let rows = h.claim_rows();
        assert_eq!(
            ProjectHolders::from_rows(&h.device_rows(), &rows).claimants(99, 1),
            vec!["NEW"]
        );
    }

    #[test]
    fn rows_round_trip_to_an_equal_map_including_claimless_devices() {
        let mut h = ProjectHolders::from_snapshot(&snap());
        // PUB ends up with no claims at all — still a known device
        h.apply_delta(&HolderDeltaWire {
            device: "PUB".into(),
            add: vec![],
            rm: vec![1],
        });
        let back = ProjectHolders::from_rows(&h.device_rows(), &h.claim_rows());
        assert_eq!(back, h);
    }

    #[test]
    fn a_new_version_held_only_by_the_offline_publisher_is_waiting_for_it() {
        let h = ProjectHolders::from_snapshot(&snap());
        let offs: HashSet<String> = ["OFF".to_string()].into();
        let f = FrameRef {
            project_id: "p1",
            frame_seq: 1,
            content_version: 2,
            publisher_devices: &offs,
        };
        let mut p = PresenceBook::default();
        p.apply_hello("p1", &[]);
        assert!(!waiting_for_publisher(&h, &p, &members(), "ME", &f)); // PUB also holds v2
        let mut h2 = h.clone();
        h2.apply_delta(&HolderDeltaWire {
            device: "PUB".into(),
            add: vec![],
            rm: vec![1],
        });
        h2.apply_delta(&HolderDeltaWire {
            device: "ME".into(),
            add: vec![],
            rm: vec![1],
        });
        assert!(waiting_for_publisher(&h2, &p, &members(), "ME", &f));
        // the publisher comes online → no longer waiting
        p.apply_hello(
            "p1",
            &[PresenceEntry {
                device: "OFF".into(),
                serving: true,
                relay_url: None,
            }],
        );
        assert!(!waiting_for_publisher(&h2, &p, &members(), "ME", &f));
    }
}
