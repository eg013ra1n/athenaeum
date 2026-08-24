# Security Policy

## Supported versions

Athenaeum is pre-1.0 and moves quickly. Only the latest release gets fixes.

## Reporting a vulnerability

Please do not open a public issue.

Use GitHub's private reporting — the **Security** tab → **Report a
vulnerability** — or email <vilen.sharifov@gmail.com>.

Include what you did, what happened, and what you expected. A proof of concept
helps but is not required.

You can expect an acknowledgement within a few days. Once a fix ships you are
credited in the release notes unless you would rather not be.

## Areas worth extra scrutiny

- `crates/athenaeum-core/src/sharing/` — the iroh peer-to-peer transport and its
  wire protocol.
- `crates/athenaeum-core/src/sync/` — device-to-device transfers, including what
  a peer is allowed to send and where it lands on disk.
- `crates/athenaeum-core/src/account/` — account tokens and their storage.
- `crates/athenaeum-web/` — the HTTP surface, when it is exposed beyond
  localhost.
- Path handling in `file_op/`, `archive/` and `scanner/` — these move and delete
  real files.
