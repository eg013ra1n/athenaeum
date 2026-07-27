# Perseus Mirror Hierarchy — design (2026-07-27)

Receivers of Perseus transfers expect files to arrive in the same directory
hierarchy they occupy under the capture root. Today the hierarchy *is* carried
per batch (`compute_rel_path` in `crates/perseus/src/run.rs` records each file's
capture-relative, forward-slash path in the manifest), but the receiver lands
every transfer in its own folder — `<incoming>/<sender_slug>/<batch_slug>/<rel_path>`
(`sync/receiver.rs::resolve_landing_dir`) — so files from one capture directory
sent across different batches scatter into different `<batch_slug>` trees.

This feature adds an opt-in **mirror** layout: a Perseus setting that makes all
of its sends land in ONE stable tree per sender —
`<incoming>/<sender_slug>/<rel_path>` — a one-way, additive sync toward the
receiver. Send one file by hand, and when its directory siblings fly later
(auto batch, scheduled fire, another manual send) they land next to it.

Decisions ratified during brainstorming:

- **Sender decides.** The setting lives on Perseus; the receiver honors the
  wire-carried layout. One knob at the observatory governs every receiving
  device identically.
- **Collisions suffix, never overwrite.** A same-`rel_path` file with different
  content lands as `name_2.fits` (byte-identical repeats are killed by the
  dedup handshake and never reach landing). Nothing is ever destroyed by an
  autonomous agent.
- **Scope: every Perseus send path** — auto/scheduled batches, Library manual
  send, send-to-device. Desktop senders are untouched in v1 (the wire flag is
  generic; they can adopt it later).

## §1 Setting + per-transfer stamping (Perseus)

`mirror_hierarchy` (default **ON** — owner decision 2026-07-27 post-ship; per-batch folders are the opt-out) as a top-level key in the Perseus
TOML + `config_template.toml` (the send keys — `mode`, `auto_quiet_secs`,
`schedule_times`, `schedule_catchup` — are all top-level; there is no `[send]`
table), surfaced as a checkbox in the web UI's To-Sync strip send-mode editor —
the same panel that owns the 3-way mode radio (T14) — ("Mirror capture folder
hierarchy on receiver") with a short hint line. Edited
through the existing one-atomic-edit send-config PUT (T14 machinery) and the
supervisor send-config reconcile seam (S3), so a hand-edited TOML reaches the
running batcher without restart.

The flag is **stamped per transfer at enqueue time**: new column
`sync_outbound.layout` (`'batch'` | `'mirror'`, guarded-`ALTER TABLE` default
`'batch'`). All three Perseus send paths flow through the same enqueue, so one
stamp point covers them. Semantics:

- A resend (same row, `generation`+1) keeps its row's stamped layout — an
  attempt is the same transfer.
- `resend_declined_as_new_transfer` clones the stamped layout onto the new row
  (the new transfer is a retry of the same intent, not a new decision point).
- Flipping the setting affects only transfers enqueued afterwards; in-flight
  and armed-retry transfers keep their stamp.

## §2 Wire

Append-only per the frozen-postcard rule (`sharing/iroh/proto.rs`):
`Msg::Announce4(PackageAnnounceV4)` appended as the LAST variant —
`PackageAnnounceV3`'s fields plus `layout` (enum `PackageLayout { Batch,
Mirror }`, postcard-stable). Golden pins added in
`sharing/wire_golden_tests.rs`; no existing variant/field moves.

**The sender emits `Announce4` only when the transfer's layout is `mirror`.**
Batch-layout transfers keep emitting `Announce3`, so users who never enable the
setting have zero compatibility exposure. An old receiver getting `Announce4`
fails to decode → the announce is never acked → the sender retries on its
normal schedule. This is the same both-sides-upgrade stance as the
Announce2→Announce3 rollout; the end-user docs state the receiver version
requirement. No sender-side capability probe in v1.

Receiver mapping (`announce_received_from_msg`): v1–v3 announces map to
`layout = Batch`; `Announce4` carries its layout through.
`TransportEvent::AnnounceReceived` gains a `layout` field, and the loopback
mock carries it exactly as the real transport does — no more, no less
(mock-parity rule).

## §3 Landing (receiver)

The single behavioral change is in `handle_announce` (`sync/receiver.rs`),
which realizes the mirror layout by reusing the already-tested pre-v2 (v1)
landing path: the receiver computes a `landing_override` only for a NAMED
batch; when there is none, ingest lands under `<incoming_root>/<sender_slug>/<rel_path>`
directly — which IS the mirror tree.

- `layout == Mirror` → `landing_override = None`: the `<batch_slug>` layer and
  its active-claim `_2`/`_3` directory suffix loop are skipped entirely
  (concurrent mirror transfers from one sender MUST share the tree — that is
  the feature); `resolve_landing_dir` is not called and is not modified. The
  inbound row's `landing_dir` stays NULL (exactly like a v1 announce), and
  ingest recomputes `<incoming_root>/<sender_slug>` per attempt — a sender
  device rename between attempts moves the tree, same as v1 (accepted,
  pre-existing behavior).
- `layout == Batch` → byte-for-byte today's behavior (named batches still go
  through `resolve_landing_dir`).

`land_payload` (`sync/ingest.rs`) is **not modified**: it already does
`create_dir_all` on the joined parent, tmp + atomic rename, and per-file
`unique_path` collision suffixing — which is exactly the ratified per-file
collision semantics. `rel_path` stays `validate_rel_path`-guarded (no escape),
and multi-capture-root sends already prefix a sanitized root label as the first
segment, giving `<sender_slug>/<root-label>/<tree>` — collision-free across
roots. Forward-slash rel_paths join correctly on Windows.

## §4 Honest boundaries (what does NOT change)

- **History deletion never touches the tree.** Deleting a received transfer's
  history releases blob tags only (`delete_transfer_history` reclaims
  `Reclaim::Tags` for received rows); landed files are the user's data and are
  never deleted. There is therefore no shared-tree deletion hazard.
- **Dedup is content-based.** A file already received earlier (e.g. into an
  old per-batch folder) will not re-materialize in the mirror tree on a
  re-send — the handshake reports it already on the peer (consistent with the
  Black-Hole-counts-as-presence decision). No migration of previously landed
  batches.
- **Additive one-way sync.** Source deletions and renames do not propagate. A
  rename on the capture side re-sends under the new rel_path; the old landed
  file stays.
- **Failure semantics unchanged.** Decline/cancel before ingest lands nothing;
  a mid-ingest failure leaves the already-landed files in place — identical to
  today's per-batch behavior.

## §5 Testing

- Unit, receiver: mirror announce produces `landing_override = None` (no batch
  slug, no dir-suffix loop, `landing_dir` NULL on the row; batch arm
  byte-identical — the existing `resolve_landing_dir` tests keep passing
  untouched), landing shared by two concurrent mirror rows.
- Unit, wire: `Announce4` golden pin; v1–v3 → `layout = Batch` fallback pin;
  loopback parity for the new field.
- Unit, Perseus: stamp-at-enqueue for all three send paths; resend keeps the
  stamp; `resend_declined_as_new_transfer` clones it; config
  reconcile applies a TOML flip to the next enqueue only.
- Integration (loopback): two sequential batches from one capture dir land
  adjacent in one tree; a changed-content collision lands as `name_2.fits`.
- Owner e2e smoke: Perseus → desktop, two manual sends from one directory with
  the setting ON; flip OFF and confirm per-batch layout returns.

## §6 Documentation

Folds into the already-recorded end-user docs TODO (artfrom-space): Perseus
guide + transfers page gain the mirror-layout section — what it does, the
receiver minimum version, additive semantics (no deletes/renames), `_2`
collision naming, and the note that previously received per-batch files do not
migrate.
