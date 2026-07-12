# Sync Dedup Handshake (Plan 3) — Design — 2026-07-12

**Status:** design (brainstormed with owner 2026-07-12). **Repos touched:** `athenaeum` (core only; Perseus + app both consume the engine change). **Refines** §7 of `docs/superpowers/specs/2026-07-11-sync-model-phase1-design.md` with the client-side decisions left open there, now that the multi-target fan-out + cleanup coordinator (Plan 2C Task 7) exist.

This is **Plan 3 of Sync Phase 1** (the last piece of the model). Plans 1 (hub), 2A/2B/2C (client) are done; deploy is held for the joint hub ship.

---

## 1. Goal

Never *transfer* a frame the receiver already has. A P2P **offer/want handshake** runs before the existing `Announce`, so each receiver fetches only the blobs it is missing. The hub stays data-free (the exchange is peer-to-peer). This is a **bandwidth optimization and is best-effort** — correctness never depends on it; the receiver's ingest-time uuid/content dedup remains the safety net.

Non-goals: any hub involvement; changing the ingest/ack path; a full-library hash index (the sampling hash already covers the whole catalog at zero extra cost).

---

## 2. Resolved decisions (owner, 2026-07-12)

- **Build model — minimal / "serve only the want set".** Keep Plan 2C Task 7's *build-once* package. Each per-peer engine runs the handshake with its target, then **imports only its want subset into its own blob store and announces a want-subset collection** (content-addressed over the shared payloads — no blob duplication). The Task 7 shared-dir **cleanup coordinator is unchanged**. This dedups the *transfer* (bandwidth) with no sender-disk regression and no rework of the just-reviewed Task 7. (The "build only the want-set payloads" phrasing in the Phase-1 §7 — which would also save sender disk but reworks Task 7 — is explicitly the rejected alternative.)
- **Placement — in the `SyncEngine` per-peer send path.** Because each Perseus target is its own engine, multi-target dedup falls out for free (each engine negotiates independently), and future app→app send (Phase 3) inherits it with no extra work.
- **Version skew — in-band best-effort fallback, no ALPN bump.** The sender sends `Offer` first; if it receives a valid `Want`, it proceeds with dedup; if the peer errors/closes/times out on the `Offer` (an older receiver that expected `Announce` as the first message), the sender **falls back to announcing the full package** (current behavior). Correctness is unaffected either way. No `athenaeum/sync/2` ALPN is introduced.

---

## 3. Protocol (additive to `Msg`, `sharing/iroh/proto.rs`)

Three new variants on the existing `athenaeum/sync/1` ALPN, exchanged **before** `Announce`. postcard-encoded like the existing `Announce`/`Ack`.

1. **`Offer { package_id, entries: Vec<OfferEntry> }`** (provider → receiver), `OfferEntry { rel_path: String, sampling_hash: String, byte_size: u64 }`.
   `sampling_hash` = `duplicates::compute_xxhash` (first/middle/last 512 KB) — the same value the receiver already stores in `files.content_hash` for every catalog file. Sender source of the value: **app** reads the frame's `files.content_hash` from its DB; **Perseus** computes it on the capture file (`compute_xxhash`).
2. **`Want { package_id, want: Vec<String>, candidates: Vec<String> }`** (receiver → provider), keyed by `rel_path`.
   Receiver split, per offered entry: **no** `sampling_hash` match in `files.content_hash` → `want` (definitely absent); **a** match → `candidate` (possible duplicate — must confirm, because the sampling hash is lossy and a blind skip could silently drop a genuinely-new file).
3. **`FullHashes { package_id, entries: Vec<FullHashEntry> }`** (provider → receiver, candidates only), `FullHashEntry { rel_path: String, xxh3_full: String }`.
   The manifest already carries the full `xxh3` per frame, so this is free to assemble. Receiver re-hashes its own matching catalog file(s) with full `xxh3`: a **true** match is dropped; a **false** positive is moved into `want`. The receiver replies with a final `Want` (want-only; empty candidates). The **final want set** is what gets served/transferred.

If `candidates` is empty, the `FullHashes` round is skipped and the first `Want` is final.

---

## 4. Send flow (per engine, `SyncEngine`)

Replaces the current "enqueue → `Announce` → serve → `Ack`" head with:

```
enqueue (full package built once, Task 7)
  → open control stream to peer
  → send Offer(package_id, entries from the manifest)
  → recv Want
      → if candidates non-empty: send FullHashes(candidates) ; recv refined Want
  → final want set W
      → if the peer never returned a valid Want (old version / error / timeout):
            FALL BACK — announce the full package (current path), done.
      → else: import only W's blobs into this engine's blob store,
              build a want-subset collection (root_hash over W),
              Announce(root_hash for W) → serve → receiver fetches only W → Ack
  → batch outcome: { new: |W|, duplicate: |offered| - |W| }
```

- `W` empty → **nothing to transfer**: record the package terminal (all-duplicate) with a `{new:0, duplicate:all}` outcome, emit the finished event, no `Announce`. This is a legitimate success, not a failure.
- The **receiver** ingest/ack path is unchanged: it still fetches the announced collection and acks per-frame receipts. It only gains the offer/want responder before the announce.
- Cleanup coordinator (Task 7): unchanged. A per-peer engine still reaches a terminal state (confirmed / all-duplicate / failed / cancelled) and reports it to the coordinator; the shared package dir is cleaned when all delivered targets are terminal.

---

## 5. Batch outcome

The engine returns/records `{ new, duplicate }` per package for the approve/history surfaces (Phase 2). `duplicate` = offered − final-want (both true duplicates and sampling-collision-confirmed). This is UI/telemetry data, not a log; it rides the existing progress/finished events (add the counts to the finished payload), never the hub.

---

## 6. Data model & interfaces summary

**Core (`athenaeum-core`), no hub/db-schema change:**
- `sharing/iroh/proto.rs` — `Msg::{Offer, Want, FullHashes}` + `OfferEntry`/`FullHashEntry`; postcard round-trip tests.
- The transport control-stream handling (`sharing/iroh/mod.rs` + the loopback mock `sharing/loopback.rs`) carries the new messages both ways, mirroring how `Announce`/`Ack` are carried, so the engine behaves identically over iroh or loopback.
- `sync/engine.rs` — the send-flow head (§4): offer-build from the manifest, want negotiation, best-effort fallback, want-subset import+collection, all-duplicate terminal, `{new,duplicate}` outcome.
- `sync/receiver.rs` — the offer/want responder: `sampling_hash → files.content_hash` lookup (want vs candidate split), full-hash confirm for candidates.
- Receiver dedup query: a `db` helper `frames_with_content_hash(conn, &[sampling_hash]) -> map` (indexed on `files.content_hash`; the index exists).
- `sync/dedup.rs` (new, small) — the pure split logic (`partition_offer(offered, local_sampling_set) -> (want, candidates)` and `confirm_candidates(candidates, sender_full, local_full) -> (dropped, still_want)`) so it is unit-testable without a transport.

**Clients:** none beyond consuming the engine change — Perseus and the (future) app send path both go through `SyncEngine`.

---

## 7. Security & correctness

- **Path safety:** unchanged — `rel_path` is still `validate_rel_path`-guarded on both sides (the offer/want carry `rel_path` but nothing is written from them; landing still uses the validated manifest `rel_path`). The offer/want never widen the write surface.
- **No data loss from dedup:** a sampling-match is only skipped after a **full-hash** confirm; a false positive is transferred; and ingest-time dedup is the final net. An all-duplicate package is a real success (the frames are already on the receiver).
- **Best-effort:** any handshake failure degrades to a full send, never to a dropped frame.
- **Hub privacy invariant preserved:** the exchange is P2P; the hub never sees hashes, rel_paths, or batch composition.
- **Interaction with the cleanup coordinator (Task 7):** the want-subset serve does not change *when* a package is terminal per peer, so the all-targets-terminal cleanup gate holds. An all-duplicate (empty-want) package terminalizes immediately — it still reports terminal to the coordinator, so it can't pin the shared payload.

---

## 8. Testing strategy

TDD; gates as usual (`cargo build --workspace`, core tests, `sync_e2e`). Key tests:
- **`sync/dedup.rs` unit:** absent entry → want; sampling-match true-duplicate (full hashes equal) → dropped; sampling-collision (sampling equal, full differ) → moved back to want; empty candidates → no FullHashes round.
- **proto round-trip:** Offer/Want/FullHashes postcard encode/decode.
- **engine:** all-duplicate package (W empty) terminalizes with `{new:0, duplicate:n}` and no Announce; best-effort fallback when the peer doesn't answer the Offer → full announce; a mixed batch serves only the want subset.
- **e2e (`sync_e2e`):** two instances with overlapping catalogs — a re-send of an overlapping batch transfers only the new frames; the receiver ends with the union, no duplicates.

---

## 9. Out of scope

The Phase-2 approve/history UI that *consumes* `{new,duplicate}` is a later cycle; this plan only produces the counts on the finished event. No hub changes. No app→app send UI (Phase 3). No sender-disk-saving "build only the want-set payloads" rework (rejected alternative in §2).
