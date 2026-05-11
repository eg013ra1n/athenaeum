#!/usr/bin/env bash
# Convert a small subset of CommonMark markdown on stdin to the HTML subset
# Telegram's parse_mode=HTML accepts. Output on stdout.
#
# Conversions, applied in order:
#   1. HTML-escape & < > in the source (so user text can never inject tags).
#   2. Strip "### ", "## ", "# " heading prefixes and wrap the heading text in <b>.
#   3. **bold** -> <b>bold</b>
#   4. *italic* -> <i>italic</i>      (single-asterisk only; greedy on shortest match)
#   5. `code`  -> <code>code</code>
#   6. [text](url) -> <a href="url">text</a>
# List markers ("- " and "* " at line start) are preserved as plain text;
# Telegram doesn't render bullets.
set -euo pipefail

# All conversions chained through one sed -E invocation. Order matters:
# - HTML-escape FIRST so tags we generate later aren't escaped.
# - Headings BEFORE bold so a "# **foo**" becomes "<b>**foo**</b>" then "<b><b>foo</b></b>".
#   That nested-bold case shouldn't happen in our notes, but ordering is documented.
sed -E \
  -e 's/&/\&amp;/g' \
  -e 's/</\&lt;/g' \
  -e 's/>/\&gt;/g' \
  -e 's/^### (.*)$/<b>\1<\/b>/' \
  -e 's/^## (.*)$/<b>\1<\/b>/' \
  -e 's/^# (.*)$/<b>\1<\/b>/' \
  -e 's/\*\*([^*]+)\*\*/<b>\1<\/b>/g' \
  -e 's/(^|[^*])\*([^*]+)\*([^*]|$)/\1<i>\2<\/i>\3/g' \
  -e 's/`([^`]+)`/<code>\1<\/code>/g' \
  -e 's/\[([^]]+)\]\(([^)]+)\)/<a href="\2">\1<\/a>/g'
