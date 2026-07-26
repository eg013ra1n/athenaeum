//! Cross-account project authorizer (Stage II collaboration, slice 4).
//!
//! Every decision here is read from the LOCALLY-CACHED, signature-verified
//! membership snapshots (`collab_projects.members_json`, a serialized
//! `Vec<SnapshotMember>` written only from hub-fetched, hub-signed snapshots —
//! the slice-3 invariant). This module never fetches, never verifies signatures
//! (slice 3 already did), and never trusts a peer-supplied claim: a node is a
//! member of a project iff one of that project's cached members lists the node's
//! raw ed25519 pubkey.
//!
//! **Fail-closed, always.** A missing project row, a `members_json` that does not
//! parse, a DB read error, or simply no matching node ⇒ `None` / `false`. Node
//! matching decodes each member's base64 `nodes[]` into 32 raw bytes and compares
//! those bytes to the [`NodeId`] — never the base64 strings (two different base64
//! encodings could name the same key). Malformed node entries are skipped with a
//! single `tracing::warn!` per call.

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;

use crate::collab::snapshot::SnapshotMember;
use crate::sharing::types::NodeId;

/// The membership facts about one project member, resolved from the cached
/// snapshot. Enough to answer every serve/announce authorization question
/// without re-reading the row.
#[derive(Debug, Clone, PartialEq)]
pub struct MemberIdentity {
    pub display_name: String,
    /// `"send"` (contributor: push-seed only) or `"send_receive"` (may pull).
    pub data_role: String,
    pub coordinator: bool,
}

/// The member (if any) that `node` belongs to in `project_id`'s cached snapshot.
/// Fail-closed: no row / parse error / no match ⇒ `None`.
pub fn member_for_node(
    conn: &rusqlite::Connection,
    project_id: &str,
    node: &NodeId,
) -> Option<MemberIdentity> {
    let row = match crate::db::collab::get_project(conn, project_id) {
        Ok(Some(row)) => row,
        Ok(None) => return None,
        Err(e) => {
            tracing::warn!(project_id, error = %e, "collab authz: project read failed; deny");
            return None;
        }
    };
    let members = parse_members(&row.members_json);
    let mut warned = false;
    members
        .into_iter()
        .find(|m| member_owns_node(m, node, &mut warned))
        .map(|m| MemberIdentity {
            display_name: m.display_name,
            data_role: m.data_role,
            coordinator: m.coordinator,
        })
}

/// True when `node` appears in ANY cached project snapshot. This is the
/// connect-gate feed: a peer is allowed to open a connection when it is a member
/// of at least one project I know about. Fail-closed: a DB error ⇒ `false`.
pub fn node_in_any_project(conn: &rusqlite::Connection, node: &NodeId) -> bool {
    let projects = match crate::db::collab::list_projects(conn) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(error = %e, "collab authz: project list failed; deny");
            return false;
        }
    };
    let mut warned = false;
    projects
        .iter()
        .any(|row| parse_members(&row.members_json).iter().any(|m| member_owns_node(m, node, &mut warned)))
}

/// May `node` be SERVED `package` of `project_id`? A `send_receive` member or the
/// coordinator may be served a published package; a still-pending package may be
/// served ONLY to the coordinator (they decide it). Fail-closed on an unknown
/// project / non-member.
pub fn may_serve_package(
    conn: &rusqlite::Connection,
    project_id: &str,
    package_pending: bool,
    node: &NodeId,
) -> bool {
    match member_for_node(conn, project_id, node) {
        Some(id) if package_pending => id.coordinator,
        Some(id) => id.coordinator || id.data_role == "send_receive",
        None => false,
    }
}

/// May an inbound PROJECT announce from `node` for `project_id` be accepted? Any
/// current member role qualifies — a send-only contributor push-seeds its frames
/// to the swarm, so it too must be allowed to announce. Fail-closed on a
/// non-member.
pub fn may_accept_announce(conn: &rusqlite::Connection, project_id: &str, node: &NodeId) -> bool {
    member_for_node(conn, project_id, node).is_some()
}

/// Parse the cached `members_json` blob (a serialized `Vec<SnapshotMember>`).
/// A parse failure is fail-closed: it yields an empty member list (nobody is
/// authorized), logged once.
fn parse_members(members_json: &str) -> Vec<SnapshotMember> {
    match serde_json::from_str::<Vec<SnapshotMember>>(members_json) {
        Ok(members) => members,
        Err(e) => {
            tracing::warn!(error = %e, "collab authz: members_json parse failed; treating as empty");
            Vec::new()
        }
    }
}

/// Does `member` list `node` among its device pubkeys? Each `nodes[]` entry is a
/// base64-encoded 32-byte ed25519 pubkey; decode it and compare the RAW bytes to
/// `node`. Malformed entries (not base64, or not 32 bytes) are skipped, warning
/// at most once per call via `warned`.
fn member_owns_node(member: &SnapshotMember, node: &NodeId, warned: &mut bool) -> bool {
    for entry in &member.nodes {
        match B64.decode(entry) {
            Ok(bytes) => match <[u8; 32]>::try_from(bytes.as_slice()) {
                Ok(pubkey) => {
                    if &pubkey == node {
                        return true;
                    }
                }
                Err(_) => warn_once(warned, "collab authz: member node pubkey is not 32 bytes; skipping"),
            },
            Err(_) => warn_once(warned, "collab authz: member node pubkey is not valid base64; skipping"),
        }
    }
    false
}

/// Emit `msg` at `warn` at most once per authorization call (flips `warned`).
fn warn_once(warned: &mut bool, msg: &'static str) {
    if !*warned {
        tracing::warn!("{msg}");
        *warned = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::collab::{upsert_project, CollabProjectRow};
    use rusqlite::Connection;

    const NODE_A: NodeId = [0xAA; 32];
    const NODE_B: NodeId = [0xBB; 32];
    const STRANGER: NodeId = [0xCC; 32];

    fn test_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        conn
    }

    /// Two members: a coordinator with `send_receive` owning node A, and a
    /// send-only contributor owning node B. Encoded EXACTLY as slice-3 writes
    /// the cache (`SnapshotMember` camelCase, base64 32-byte node pubkeys).
    fn two_member_json() -> String {
        serde_json::json!([
            {
                "accountId": "acc-a",
                "displayName": "Alice",
                "dataRole": "send_receive",
                "coordinator": true,
                "nodes": [B64.encode(NODE_A)]
            },
            {
                "accountId": "acc-b",
                "displayName": "Bob",
                "dataRole": "send",
                "coordinator": false,
                "nodes": [B64.encode(NODE_B)]
            }
        ])
        .to_string()
    }

    fn seed_project(conn: &Connection, project_id: &str, members_json: &str) {
        let row = CollabProjectRow {
            project_id: project_id.to_string(),
            slug: format!("{project_id}-slug"),
            title: format!("Project {project_id}"),
            data_role: "send_receive".into(),
            is_coordinator: true,
            require_approval: false,
            pending_announcements: 0,
            project_status: "active".into(),
            target_name: "M101".into(),
            target_ra_deg: 210.8,
            target_dec_deg: 54.35,
            target_radius_deg: 1.5,
            membership_version: 1,
            snapshot_payload_b64: "cGF5bG9hZA==".into(),
            snapshot_signature_b64: "c2ln".into(),
            members_json: members_json.to_string(),
            thresholds_version: None,
            thresholds_rules_json: None,
            // local preference — ignored on write
            auto_replicate: true,
            fetched_at: String::new(),
        };
        upsert_project(conn, &row).unwrap();
    }

    #[test]
    fn member_for_node_resolves_each_role_and_none_for_stranger() {
        let conn = test_conn();
        seed_project(&conn, "p-1", &two_member_json());

        let a = member_for_node(&conn, "p-1", &NODE_A).expect("node A resolves");
        assert_eq!(
            a,
            MemberIdentity {
                display_name: "Alice".into(),
                data_role: "send_receive".into(),
                coordinator: true
            }
        );
        let b = member_for_node(&conn, "p-1", &NODE_B).expect("node B resolves");
        assert_eq!(
            b,
            MemberIdentity {
                display_name: "Bob".into(),
                data_role: "send".into(),
                coordinator: false
            }
        );
        assert!(member_for_node(&conn, "p-1", &STRANGER).is_none());
        // Unknown project id is fail-closed too.
        assert!(member_for_node(&conn, "no-such-project", &NODE_A).is_none());
    }

    #[test]
    fn may_serve_package_published_needs_send_receive_or_coordinator() {
        let conn = test_conn();
        seed_project(&conn, "p-1", &two_member_json());

        // Published (not pending): A (coordinator + send_receive) yes, B (send
        // only) no, stranger no.
        assert!(may_serve_package(&conn, "p-1", false, &NODE_A));
        assert!(!may_serve_package(&conn, "p-1", false, &NODE_B));
        assert!(!may_serve_package(&conn, "p-1", false, &STRANGER));
    }

    #[test]
    fn may_serve_package_pending_is_coordinator_only() {
        let conn = test_conn();
        seed_project(&conn, "p-1", &two_member_json());

        // Pending: only the coordinator (A). B is send_receive-less anyway, but
        // even a send_receive non-coordinator would be refused a pending package.
        assert!(may_serve_package(&conn, "p-1", true, &NODE_A));
        assert!(!may_serve_package(&conn, "p-1", true, &NODE_B));
        assert!(!may_serve_package(&conn, "p-1", true, &STRANGER));
    }

    #[test]
    fn may_accept_announce_allows_any_member() {
        let conn = test_conn();
        seed_project(&conn, "p-1", &two_member_json());

        // Both members (send-only B included — it push-seeds) may announce; a
        // stranger may not.
        assert!(may_accept_announce(&conn, "p-1", &NODE_A));
        assert!(may_accept_announce(&conn, "p-1", &NODE_B));
        assert!(!may_accept_announce(&conn, "p-1", &STRANGER));
    }

    #[test]
    fn node_in_any_project_scans_every_snapshot() {
        let conn = test_conn();
        seed_project(&conn, "p-1", &two_member_json());

        assert!(node_in_any_project(&conn, &NODE_A));
        assert!(node_in_any_project(&conn, &NODE_B));
        assert!(!node_in_any_project(&conn, &STRANGER));
    }

    #[test]
    fn empty_table_is_fail_closed() {
        let conn = test_conn();
        // No projects cached at all: every question answers deny.
        assert!(member_for_node(&conn, "p-1", &NODE_A).is_none());
        assert!(!may_serve_package(&conn, "p-1", false, &NODE_A));
        assert!(!may_serve_package(&conn, "p-1", true, &NODE_A));
        assert!(!may_accept_announce(&conn, "p-1", &NODE_A));
        assert!(!node_in_any_project(&conn, &NODE_A));
    }

    #[test]
    fn malformed_nodes_and_bad_json_are_skipped_not_fatal() {
        let conn = test_conn();
        // A member with a non-base64 node and a wrong-length node, plus one good
        // node B: the malformed entries are skipped, B still resolves.
        let json = serde_json::json!([
            {
                "accountId": "acc-b",
                "displayName": "Bob",
                "dataRole": "send",
                "coordinator": false,
                "nodes": ["not-base64!!!", B64.encode([1u8; 16]), B64.encode(NODE_B)]
            }
        ])
        .to_string();
        seed_project(&conn, "p-1", &json);
        assert!(may_accept_announce(&conn, "p-1", &NODE_B));
        assert!(!may_accept_announce(&conn, "p-1", &NODE_A));

        // A members_json that does not parse ⇒ that project authorizes nobody
        // (fail-closed), independent of any other cached project.
        seed_project(&conn, "p-2", "{ this is not a member array");
        assert!(!may_accept_announce(&conn, "p-2", &NODE_B));
        assert!(member_for_node(&conn, "p-2", &NODE_B).is_none());
    }
}
