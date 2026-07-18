# Hub Operator Console — Design

Date: 2026-07-18 · Status: approved (owner brainstorm 2026-07-18) · Repo: athenaeum-hub (+ astronet env template)

## Context

The portal ships per-project **coordinator** admin (join requests, thresholds,
settings; frame moderation is in-app). There is NO instance-level role: nobody
can see all projects, manage other people's projects, or administer accounts.
The hub owner needs an operator console for exactly that. Frame-level
moderation stays with coordinators (owner decision — the operator has no
access to frame content anyway; contributions are P2P and never touch the hub).

## Goals (v1)

1. Dashboard: instance health/stats from existing tables.
2. All-projects overview + project management (feature, hide, close, delete,
   coordinator transfer, join-request decisions on behalf).
3. Accounts & devices: list/search, account block/unblock, device revoke.
4. Project creation with an assigned coordinator.
5. Every operator mutation audited.

**Non-goals (v1):** operator frame moderation; role-management UI /
multi-tier roles (see Evolution); email notifications; new telemetry.

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
    target_kind text        NOT NULL,          -- 'project'|'account'|'device'|'join_request'
    target_id   text        NOT NULL,
    detail      jsonb,                          -- slug/title/email snapshot for post-delete forensics
    at          timestamptz NOT NULL DEFAULT now()
);
```

## API surface — `src/routes/operator.rs`, all behind `OperatorAuth`

| Route | Semantics |
| ----- | --------- |
| `GET  /api/v1/operator/overview` | counts (accounts, active devices by capability, projects active/hidden/closed, members, announcements by state, pending join requests) + recent-events feed (UNION of latest rows across projects / join_requests / announcements / devices, kind+at, limit 50). Pure SQL over existing tables. |
| `GET  /api/v1/operator/projects` | ALL projects incl. hidden/closed; per row: slug, title, coordinator email, member count, announcement count, created_at, last activity, featured/hidden/status. |
| `POST /api/v1/operator/projects` | create with `coordinator_email` — account must exist; creates the project + coordinator membership (reuses the NewProject creation path), bumps nothing else. |
| `PATCH /api/v1/operator/projects/{id}` | `featured`, `hidden` (sets/clears `hidden_at`), `status` (`active`/`closed` — existing semantics reused as-is). |
| `POST /api/v1/operator/projects/{id}/coordinator` | `{account_id}` must be an existing member; one tx: flip `is_coordinator` flags, bump `membership_version`, re-sign the membership snapshot via the SAME code path coordinator membership changes use. |
| `POST /api/v1/operator/projects/{id}/join-requests/{rid}/decide` | approve/reject — reuses the coordinator decision logic verbatim; actor goes to audit. |
| `DELETE /api/v1/operator/projects/{id}` | hard delete; FKs cascade. Devices keep their local data (P2P); clients see "project not found" on next refresh — existing missing-project behavior. Audit `detail` snapshots slug/title/member count first. |
| `GET  /api/v1/operator/accounts?search=` | accounts with created_at, device count, project count, blocked_at. |
| `POST /api/v1/operator/accounts/{id}/block` / `unblock` | sets/clears `blocked_at`. Auth middleware rejects bearer tokens AND OTP sign-in of blocked accounts (401) — checked centrally where the account is resolved. Memberships untouched. Operator accounts cannot block themselves. |
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

## Testing

Red-proven per hub conventions: extractor (non-operator 403, operator ok,
empty allowlist inert); blocked account 401 on token AND OTP paths (+ cannot
self-block); coordinator transfer bumps `membership_version` + re-signed
snapshot verifies; delete cascades + audit snapshot row; hidden project
absent from public Directory but present in operator listing; S1 raw-body
test on operator device/account responses; portal `/operator` gated by
`/me.operator`.

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
