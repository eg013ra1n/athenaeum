# Pre-publication secrets audit

Date: 2026-08-24
Scope: `athenaeum` and `solvemyastro`, full git history.
Gate for: `docs/superpowers/specs/2026-08-24-open-sourcing-github-design.md`

Publication is irreversible — forks, archives and GitHub's caches keep whatever
is pushed. This is the record that the gate was actually run.

## Tooling

- `gitleaks` 8.30.1, default ruleset, `gitleaks git` over full history.

  `gitleaks git` walks every commit including `HEAD`, so it covers the working
  tree as published. A filesystem scan (`gitleaks dir .`) was deliberately not
  used: it descends into `target/`, `node_modules/`, `dist/` and `builds/`,
  none of which is tracked or published, and on this machine `target/` alone is
  hundreds of gigabytes.

- Preliminary hand scans, recorded here because they cover a different axis than
  gitleaks' rules:
  - Filename scan over all 18 080 reachable objects for `.env`, `.p12`, `.pem`,
    `.p8`, `.key`, `.pfx`, `id_rsa`, `id_ed25519`, `.netrc`, `.kdbx`,
    `.keychain`. One hit, `crates/athenaeum-core/src/account/token_store.rs` —
    a source file, not a credential.
  - Content scan over all reachable blob content (~264 MB) for private-key
    headers, `ghp_`/`gho_`/`ghu_`/`ghs_`/`ghr_`, `glpat-`, `xox[baprs]-`,
    `AKIA…`, Telegram bot tokens, `sk-…` and Discord webhook URLs. Zero matches.

## Results

| Scan | Result |
| ---- | ---- |
| athenaeum, full history — 1609 commits, 62.79 MB of diff content | 1 finding, triaged below as a false positive |
| solvemyastro, full history — 48 commits, 574 KB | no leaks found |
| rustafits pin `72aca7c` advertised by the public remote | yes — `refs/heads/main` and `HEAD` |

### The single finding — false positive

| Field | Value |
| ---- | ---- |
| Rule | `generic-api-key` |
| File | `src/contexts/NotificationContext.tsx:114` |
| Commit | `67e08526`, 2026-05-16, "feat(notifications): global notification center with persistent history" |
| Match | `const STORAGE_KEY = 'athenaeum.notifications.v1';` |
| Entropy | 3.74 |

Not a credential. It is the `localStorage` key the notification centre persists
its history and dedupe set under — the same string `CLAUDE.md` documents in the
Notifications section. The rule fires on the identifier containing `KEY` plus
the entropy of a dotted version string. No action taken; no allowlist entry
added, so a future scan will report it again and this record explains why.

## Accepted disclosures

Decided in the design, section 3. Do not re-flag these.

- `192.168.31.208:9080` — the LAN address of the private GitLab, in
  `.gitmodules`, `solvemyastro/Cargo.toml` and docs. Not routable from outside;
  the two entries that are real URLs are repointed to GitHub in Tasks 3 and 4 of
  the plan.
- `.gitlab-ci.yml` and `.gitlab/` reveal the deploy host, artefact paths, the
  Docker Hub repository and the notification scripts. All credentials are CI
  variables; the layout is published on purpose.
- Author identities `sharifov.v@mail366.com` (78 commits) and
  `Administrator <gitlab_admin_ba4842@example.com>` (3 commits) become public.
- Client-side service endpoints: `test-hub.artfrom.space` (debug) and
  `projects.artfrom.space` (release) in `settings/mod.rs`, the relay hosts in
  `examples/relay_check.rs`, `artfrom.space/catalogs/` in
  `catalog/gaia_prebuilt.rs`. The server code behind them is not published.

## Verdict

**Clear to publish.** Both repositories are free of credential material across
their full history. The one gitleaks finding is a `localStorage` key name.
