# Release-Notes Chat Notifications (Discord + Telegram)

**Status:** Approved
**Date:** 2026-05-11
**Scope:** GitLab CI only — no app code changes.

## Goal

When a tag matching `v*` is pushed to GitLab, post the contents of `RELEASE_NOTES.md` to a Discord channel and a Telegram chat as part of the same pipeline that builds and deploys the release. Stable and beta tags both post; the post is visually distinguishable.

## Non-Goals

- Posting to any third platform (Slack, Mastodon, X, …).
- Generating or editing release notes — `RELEASE_NOTES.md` is authored by hand as it already is.
- Cross-posting from artfrom-space's blog into chat.
- Reposting on tag re-pushes other than what GitLab's pipeline retry behaviour already gives us (each tag-push fires the pipeline once; re-pushing the same tag re-fires).

## Pipeline Shape

Two new jobs in the existing `release` stage of `.gitlab-ci.yml`:

```text
build:linux ─┐
build:windows├─→ deploy ─→ release ──┬─→ notify:discord
build:macos ─┘                       └─→ notify:telegram
                          publish_version  (already exists, parallel to release)
```

- Both new jobs: `stage: release`, `tags: [linux]`, `only: tags`, `needs: [release]`.
- Depending on `release` (not `deploy`) means the GitLab Release page is live before we link to it from chat.
- Two separate jobs (not one) so a Discord outage doesn't hide a Telegram failure and vice versa, and each can be retried independently from the GitLab UI.

## Body Source & Formatting

**Source of body text:** `RELEASE_NOTES.md` at the repo root — same file the existing `release` job reads. If the file is missing or empty, both jobs fall back to a one-line body: `Athenaeum ${CI_COMMIT_TAG} is out — see the release page for details.`

**Title:**

- Stable tag: `Athenaeum ${CI_COMMIT_TAG} released`
- Beta tag: `Athenaeum ${CI_COMMIT_TAG} (beta) released`

Beta detection reuses the pattern already in `deploy` and `publish_version`:

```sh
if echo "${CI_COMMIT_TAG}" | grep -q '\-beta'; then …
```

**Truncation:** Discord embed descriptions and Telegram messages are both capped at 4096 characters. We truncate at **3900 characters** (leaving room for the truncation tail) and append:

```text
…

Full notes: https://gitlab.com/<group>/<project>/-/releases/<tag>
```

Implemented as `head -c 3900 RELEASE_NOTES.md` plus a `wc -c` check. No Python needed for the truncation itself.

**JSON escaping** (for both jobs' `curl --data` payloads): same trick the existing `release` job already uses —

```sh
python3 -c 'import sys,json; print(json.dumps(sys.stdin.read()))'
```

`python3` is already required by the `release` job, so it's already on the linux runner.

## Discord Job

**Job name:** `notify:discord`

**Endpoint:** `${DISCORD_WEBHOOK_URL}` (full webhook URL stored as a CI variable)

**Payload:** single embed —

```json
{
  "embeds": [
    {
      "title": "Athenaeum vX.Y.Z released",
      "url": "https://gitlab.com/<group>/<project>/-/releases/vX.Y.Z",
      "description": "<truncated RELEASE_NOTES.md>",
      "color": 3066993,
      "fields": [
        {
          "name": "Download",
          "value": "[artfrom.space/releases/download](https://artfrom.space/releases/download/)"
        }
      ]
    }
  ]
}
```

- `color`: `3066993` (green `#2ecc71`) for stable, `15976499` (amber `#f39c12`) for beta — quick visual cue in the channel timeline.
- `url` resolves at runtime from `${CI_PROJECT_URL}/-/releases/${CI_COMMIT_TAG}` (no hard-coded group/project).
- Description carries the markdown body. Discord renders standard markdown (`**bold**`, `*italic*`, `# heading`, fenced code, links) directly inside an embed description, so no preprocessing is needed.

## Telegram Job

**Job name:** `notify:telegram`

**Endpoint:** `https://api.telegram.org/bot${TELEGRAM_BOT_TOKEN}/sendMessage`

**Payload:**

```json
{
  "chat_id": "${TELEGRAM_CHAT_ID}",
  "parse_mode": "HTML",
  "disable_web_page_preview": false,
  "text": "<b>Athenaeum vX.Y.Z released</b>\n\n<truncated body as HTML-ish text>\n\nDownload: https://artfrom.space/releases/download/\nRelease page: https://gitlab.com/.../releases/vX.Y.Z"
}
```

We use HTML parse mode rather than MarkdownV2. HTML is far less footgunny — Telegram's MarkdownV2 requires escaping a long list of punctuation (underscore, asterisk, brackets, parens, tilde, backtick, greater-than, hash, plus, minus, equals, pipe, braces, dot, exclamation) everywhere outside code spans, and RELEASE_NOTES.md is full of those characters (dotted version numbers, parens around clarifications, dashes in lists). HTML mode only requires escaping three characters (ampersand, less-than, greater-than) and supports a small fixed tag set: bold, italic, underline, strikethrough, anchor, code, pre.

**Markdown → HTML conversion** is a tiny inline `sed` pass — release notes are skim-read in chat, so we don't need a full markdown engine:

1. Escape ampersand → `&amp;`, less-than → `&lt;`, greater-than → `&gt;` first.
2. Strip leading `#`, `##`, `###` heading markers, or wrap the heading text in a bold tag.
3. Convert `**bold**` and `*italic*` to the corresponding bold/italic tags.
4. Convert backtick-wrapped inline code to a code tag.
5. Leave `[text](url)` as an anchor tag with `href="url"`.
6. List markers (`-` or `*` at line start) stay as plain lines — Telegram doesn't render bullets natively.

The conversion lives inline in the job's script as a single `sed -E` chain. If it grows past ~5 lines, we extract to `.gitlab/ci/scripts/md_to_telegram_html.sh` (do not pre-extract — keep it inline until it actually misbehaves).

## Failure Handling

Both jobs use:

```sh
curl --silent --show-error --max-time 30 \
  --request POST \
  --header 'Content-Type: application/json' \
  --data "@payload.json" \
  "$URL" \
  || echo "WARNING: <platform> notification failed (exit $?). Pipeline continues."
```

- No `--fail`. Discord/Telegram returning 4xx or 5xx is logged but does not fail the job.
- Each job is independent — Discord failing does not prevent Telegram from running, because they're siblings under `needs: [release]`, not chained.
- The job exits `0` even on transport failure. Rationale: the build is already shipped by the time these jobs run; turning a Discord hiccup into a red pipeline forces a "retry the deploy" mental model that doesn't match reality. The warning shows up in the pipeline log if anyone looks.

## Missing-Secret Handling

If a required CI variable is empty/unset, the job exits 0 with a clear log message instead of failing:

```sh
if [ -z "${DISCORD_WEBHOOK_URL:-}" ]; then
  echo "Skipping Discord notification — DISCORD_WEBHOOK_URL not set in CI/CD variables."
  exit 0
fi
```

This means the first pipeline after merging the change does not turn red while the user is still adding the secrets.

## Required CI/CD Variables

Added by the user in **GitLab → Settings → CI/CD → Variables**, all `Masked: yes`, `Protected: yes` (so they only expose to tag jobs, which run on protected refs):

| Variable | Value |
| ---- | ---- |
| `DISCORD_WEBHOOK_URL` | Full webhook URL from Discord channel settings → Integrations → Webhooks → New Webhook → Copy URL |
| `TELEGRAM_BOT_TOKEN` | From `@BotFather` → `/newbot` (or an existing release-bot's token) |
| `TELEGRAM_CHAT_ID` | Numeric chat id, or `@channelusername` for public channels. The bot must be a member (and admin, for channels). |

The Telegram bot used here is intentionally a **separate bot** from the `telegram:configure` Claude-Code-side bot — different purpose, different audience, and re-using the user-pairing bot would surface release announcements in the wrong DM.

A short bootstrap note for the user lives at the top of `.gitlab-ci.yml` as a comment block above the two new jobs, pointing at this spec.

## Acceptance

Manual checks after the next `v*` tag is pushed:

1. Pipeline succeeds (green) on a tag push, regardless of whether secrets are set.
2. With `DISCORD_WEBHOOK_URL` set, a single embed appears in the target channel within ~30 s of `release` finishing. Title matches stable/beta. Color matches stable/beta. The "Full notes" link resolves to a real GitLab Release page.
3. With `TELEGRAM_BOT_TOKEN` + `TELEGRAM_CHAT_ID` set, a single message appears in the target chat within ~30 s. Bold title renders. Long bodies are truncated with the "Full notes" tail.
4. Unsetting either Discord or Telegram secret → that job logs the skip line and exits 0; the other platform still fires.
5. Forcing a transport error (e.g., point `DISCORD_WEBHOOK_URL` at `https://discord.com/api/webhooks/0/0`) → job logs the warning and exits 0; pipeline stays green.
6. RELEASE_NOTES.md > 4000 chars → message body ends with `…\n\nFull notes: <url>` and total length is ≤ 4096 chars (verified by counting characters of the rendered post in chat).

## Out of Scope (Future)

- Auto-generating RELEASE_NOTES.md from commit messages.
- Posting build-failure notifications to the same channels.
- Threaded replies on Discord (one embed per platform per tag is enough).
- Per-channel content variants (e.g., shorter Telegram, fuller Discord) — current approach uses the same body and trusts truncation.
