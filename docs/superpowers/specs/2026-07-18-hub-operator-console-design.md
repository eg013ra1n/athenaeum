# Hub Operator Console — Design

Date: 2026-07-18 · Status: approved (owner brainstorm + astronomer-lens review 2026-07-18) · Repo: athenaeum-hub (+ astronet env template)

## Context

The portal ships per-project **coordinator** admin (join requests, thresholds,
settings; frame moderation is in-app). There is NO instance-level role: nobody
can see all projects, manage other people's projects, or administer accounts.
The hub owner needs an operator console for exactly that. Frame-level
moderation stays with coordinators (owner decision — the operator has no
access to frame content anyway; contributions are P2P and never touch the hub).

## User stories

**Operator (the owner):**

- *Morning glance*: "what's happening on my hub?" → Dashboard counts + recent
  events.
- *Junk project*: someone created a spam/test project → hide it (out of the
  directory, members unaffected) or delete it (gone, slug-typed confirm).
- *Absent coordinator*: a project is alive, frames flow, but the coordinator
  vanished for a month → appoint a new coordinator — **any account**, not just
  an existing member (see semantics below); otherwise a dead coordinator whose
  approval gates join requests would deadlock the takeover.
- *Support call*: "I can't sign in / my code never arrives" → account row
  shows `last_seen_at`, device list with `last_seen_at`/revoked; block state
  visible. (OTP delivery inspection stays in server logs — not in v1 UI.)
- *Abuse*: a member floods a project → operator removes the member from the
  project (same guarded logic coordinators use) and/or blocks the account.
  **Honest limits**: already-approved contributions keep replicating P2P —
  retraction is the existing coordinator-side concern (ties into the
  "no un-have on the wire" follow-up); blocking cuts hub-mediated operations
  immediately, while P2P paths fade as cached peer/membership authorization
  refreshes (hourly + refusal-triggered).
- *Campaign*: operator creates an "official" M31 project and assigns a known
  astronomer as coordinator (their account must already exist; invite-by-email
  is a follow-up), marks it featured so it tops the directory.

**Astronomer (project participant) — how operator actions land on them:**

- Project hidden → my project page still works (member), new join requests are
  refused; the project just isn't advertised.
- Project closed → I can't publish new frames and nobody can join, but
  downloads/serving of approved data continue; the page says "closed".
- Project deleted → app shows the existing "project not found" state on next
  refresh; my local data and downloaded contributions stay mine (P2P).
- I got blocked → sign-in and hub calls fail with 401; when unblocked,
  everything resumes. My memberships are untouched.
- Communication about any of this happens in the project chat
  (`chat_link`) — the console does not message users (non-goal v1).

## Goals (v1)

1. Dashboard: instance health/stats from existing tables.
2. All-projects overview + project management (feature, hide, close, delete,
   coordinator appointment, join-request decisions and member removal on
   behalf of coordinators).
3. Accounts & devices: list/search with `last_seen_at`, account block/unblock,
   device revoke.
4. Project creation with an assigned coordinator.
5. Every operator mutation audited.

**Non-goals (v1):** operator frame moderation; role-management UI /
multi-tier roles (see Evolution); notifications/emails to affected users
(project chat is the human channel); **account deletion** (follow-up — note
`projects.created_by` is a plain FK, so deleting an account requires
transferring/deleting owned projects first); OTP-delivery debugging UI;
invite-by-email for coordinators.

## Operator identity — env allowlist (approach A)

- `HUB_OPERATOR_EMAILS` — comma-separated account emails in `hub.env`
  (astronet `templates/hub.env.j2` + inventory var). Empty/absent = no
  operators; console fully inert.
- **Single choke point**: an `OperatorAuth` extractor (portal session OR
  bearer, same resolution as coordinator paths) that additionally requires
  the account email ∈ allowlist (case-insensitive). ALL operator routes gate
  through it; no scattered checks.
- `/me` gains `operator: bool` (additive). Portal renders the `/operator`
  section only when true; the API remains the real gate.
- The role cannot be granted or revoked through any API — zero
  privilege-escalation surface. Changing operators = env edit + service
  restart (playbook run); acceptable for a single-owner instance.

**Future-many-moderators seams (deliberate, cheap now):** role source lives
ONLY inside `OperatorAuth`; `/me` can grow `roles: []` additively; audit
records the actor account. Upgrade path = approach B: an `account_roles`
migration, `OperatorAuth` reads the table instead of env, small
role-management page. Nothing in v1 gets rewritten (documented in Evolution).

## Migration 0012

```sql
ALTER TABLE projects ADD COLUMN featured  boolean     NOT NULL DEFAULT false;
ALTER TABLE projects ADD COLUMN hidden_at timestamptz;          -- NULL = visible
ALTER TABLE accounts ADD COLUMN blocked_at timestamptz;         -- NULL = active
CREATE TABLE operator_audit (
    id          bigserial   PRIMARY KEY,
    actor       uuid        NOT NULL REFERENCES accounts (id),
    action      text        NOT NULL,          -- e.g. 'project.delete'
    target_kind text        NOT NULL,          -- 'project'|'account'|'device'|'join_request'|'member'
    target_id   text        NOT NULL,
    detail      jsonb,                          -- slug/title/email snapshot + optional operator note
    at          timestamptz NOT NULL DEFAULT now()
);
```

## API surface — `src/routes/operator.rs`, all behind `OperatorAuth`

| Route | Semantics |
| ----- | --------- |
| `GET  /api/v1/operator/overview` | counts (accounts, active devices by capability, projects active/hidden/closed, members, announcements by state, pending join requests) + recent-events feed: the existing project-events log (`record_event_tx` rows) merged with latest account/device registrations, newest-first, limit 50. Pure SQL over existing tables. |
| `GET  /api/v1/operator/projects` | ALL projects incl. hidden/closed; per row: slug, title, coordinator email, member count, announcement count, created_at, last event at, featured/hidden/status. |
| `POST /api/v1/operator/projects` | create with `coordinator_email` — account must exist; creates the project + coordinator membership (reuses the existing creation logic with the coordinator parameterized). |
| `PATCH /api/v1/operator/projects/{id}` | `featured`; `hidden` (sets/clears `hidden_at`); `status` (`active`/`closed`). **Pinned semantics**: `closed` refuses new join requests AND new announcements (existing checks at `join_requests.rs` / `announcements.rs`); serving/downloads continue. `hidden` removes the project from the public Directory, refuses new join requests while hidden, and the portal project page answers only to members + operator (404 otherwise); members keep working. Optional `note` recorded in the audit `detail`. |
| `POST /api/v1/operator/projects/{id}/coordinator` | `{account_id}` — **any existing account**: if not yet a member, a `send_receive` membership is created in the same tx (deadlock-free takeover when the old coordinator is unreachable). One tx: membership upsert, `is_coordinator` flags flipped, `membership_version` bumped, snapshot re-signed via the SAME code path coordinator membership changes use. |
| `POST /api/v1/operator/projects/{id}/join-requests/{rid}/decide` | approve/reject — reuses the coordinator decision logic verbatim. |
| `DELETE /api/v1/operator/projects/{id}/members/{account_id}` | reuses `remove_member` logic including its guard (refuses removing the coordinator — hand over first). |
| `DELETE /api/v1/operator/projects/{id}` | hard delete; FKs cascade. Devices keep their local data (P2P); clients see "project not found" on next refresh — existing missing-project behavior. Audit `detail` snapshots slug/title/member count (+ optional `note`) first. |
| `GET  /api/v1/operator/accounts?search=` | accounts with created_at, `last_seen_at`, device count, project count, blocked_at. |
| `POST /api/v1/operator/accounts/{id}/block` / `unblock` | sets/clears `blocked_at`. Auth middleware rejects bearer tokens AND OTP sign-in of blocked accounts (401) — checked centrally where the account is resolved. Memberships untouched. Guards: cannot block yourself, cannot block any operator-allowlisted account. |
| `POST /api/v1/operator/devices/{id}/revoke` | sets `revoked_at` — existing soft-revoke semantics (device fails auth; user re-signs-in). |
| `GET  /api/v1/operator/audit?limit=` | newest-first audit rows. |

Every mutating route inserts an `operator_audit` row in the same transaction.

## Portal UI — `/operator` (visible when `me.operator`)

Tabs: **Dashboard** (stat tiles + recent events), **Projects** (table, inline
actions, featured/hidden badges), **Accounts** (search, block toggle, device
list with revoke), **Audit** (read-only table). Follows existing portal
patterns (`apiGet`/`apiPost`, section styling, same session+CSRF as existing
POSTs). Destructive actions confirm; project delete requires typing the slug.
Directory changes: `hidden_at IS NULL` filter for the public listing;
featured projects sort first with a badge.

## Security invariants

- Operator role unreachable via API (env-only); all routes behind the single
  extractor; blocked-account rejection lives where account auth is resolved.
- **S1 preserved**: operator responses never contain device direct addrs
  (device rows expose name/capability/last-seen/revoked only) — pinned by a
  raw-body substring test like the collab-portal one.
- No frame content exists on the hub; the console adds none.
- All mutations audited with actor; audit is insert-only (no delete route).
  Retention: unbounded at this scale (revisit if ever noisy).

## Testing

Red-proven per hub conventions: extractor (non-operator 403, operator ok,
empty allowlist inert); blocked account 401 on token AND OTP paths (+ cannot
block self or a fellow operator); coordinator appointment of a NON-member
creates membership + bumps `membership_version` + re-signed snapshot
verifies; member removal keeps the coordinator guard; hidden project absent
from public Directory, page 404 for non-members, join refused; closed project
refuses join + announcement but keeps serving; delete cascades + audit
snapshot row; S1 raw-body test on operator device/account responses; portal
`/operator` gated by `/me.operator`.

## Evolution — many moderators (approach B, documented not built)

`account_roles (account_id, role)` migration; `OperatorAuth` resolves from
the table (env stays as bootstrap/break-glass); `/me.roles: []` additive;
role-management page. Distinct lesser roles (e.g. hub-wide moderator without
delete/block) become rows + per-route required-capability checks inside the
same extractor family. Frame moderation stays per-project with coordinators
regardless.

## Deploy

Test-hub first per hub-deploy discipline (`hub_artifact_ref` branch soak),
prod as the separate owner procedure. `HUB_OPERATOR_EMAILS` added to
`templates/hub.env.j2` + both inventory groups (owner email on both
instances). Migration 0012 applies on boot; rollback caveat: once applied,
rolling back requires a 0012-carrying binary (same class as 0011).
