#!/usr/bin/env bash
# Rewrite the marked "Latest Build" block and insert/replace the Version
# History row of the docs site's download page.
# Env: PAGE (path), TAG, RELEASE_DATE, TAGLINE, BUILDS_BASE_URL (default
# https://artfrom.space/builds). A beta tag touches only the history row.
# Markers the page must carry:
#   <!-- latest-build:start -->  …  <!-- latest-build:end -->
#   <!-- version-history:rows -->   (the line right under the table header)
set -euo pipefail

: "${PAGE:?PAGE must be set}"; : "${TAG:?TAG must be set}"; : "${RELEASE_DATE:?RELEASE_DATE must be set}"; : "${TAGLINE:?TAGLINE must be set}"
BUILDS_BASE_URL="${BUILDS_BASE_URL:-https://artfrom.space/builds}"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
# shellcheck disable=SC1091
. "$SCRIPT_DIR/artifact_names.sh"
VERSION="${TAG#v}"
export VERSION

for marker in '<!-- latest-build:start -->' '<!-- latest-build:end -->' '<!-- version-history:rows -->'; do
  if ! grep -qF -- "$marker" "$PAGE"; then
    echo "ERROR: marker $marker not found in $PAGE" >&2
    exit 1
  fi
done

link() { # $1 = subdir, $2 = filename
  printf '[%s](%s/%s/%s/%s)' "$2" "$BUILDS_BASE_URL" "$TAG" "$1" "$2"
}
n() { artifact_name "$@"; }

block_file=$(mktemp -t latest_build.XXXXXX)
trap 'rm -f "$block_file"' EXIT
cat > "$block_file" <<EOF
### Windows

| Download | Architecture | Format |
| ---- | ---- | ---- |
| $(link windows "$(n athenaeum windows x64 msi)") | x64 (Intel / AMD) | MSI installer (recommended) |
| $(link windows "$(n athenaeum windows x64 exe setup)") | x64 (Intel / AMD) | EXE setup |

### macOS

| Download | Architecture | Format |
| ---- | ---- | ---- |
| $(link macos "$(n athenaeum macos arm64 dmg)") | Apple Silicon (M1 / M2 / M3 / M4) | DMG |
| $(link macos "$(n athenaeum macos x64 dmg)") | Intel (x86_64) | DMG |

Not sure which Mac you have? Apple menu → **About This Mac**: "Chip: Apple M…" → Apple Silicon; "Processor: Intel…" → Intel.

:::tip
macOS builds are code-signed with a Developer ID certificate and notarized by Apple — open the DMG, drag **Athenaeum** to Applications, and launch. No security workaround needed.
:::

### Linux

| Download | Architecture | Format |
| ---- | ---- | ---- |
| $(link linux "$(n athenaeum linux x64 AppImage)") | x64 | AppImage (any distro) |
| $(link linux "$(n athenaeum linux x64 deb)") | x64 | Debian / Ubuntu package |

:::tip
Every release is also archived by tag — [browse all builds](${BUILDS_BASE_URL}/) for older versions or to verify a specific release. Version-less links such as \`${BUILDS_BASE_URL}/latest/macos/$(alias_name athenaeum macos arm64 dmg)\` always point at the newest stable build.
:::
EOF

python3 - "$PAGE" "$TAG" "$RELEASE_DATE" "$TAGLINE" "$block_file" <<'PY'
import re, sys
page, tag, date, tagline, block_path = sys.argv[1:6]
text = open(page, encoding="utf-8").read()
is_beta = "-beta" in tag

if not is_beta:
    block = open(block_path, encoding="utf-8").read().rstrip("\n")
    start, end = "<!-- latest-build:start -->", "<!-- latest-build:end -->"
    a, b = text.index(start) + len(start), text.index(end)
    text = text[:a] + "\n" + block + "\n" + text[b:]

row = f"| {tag} | {date} | {tagline.replace('|', chr(92) + '|')} |"
lines = text.split("\n")
lines = [l for l in lines if not l.startswith(f"| {tag} |")]   # replace an existing row for this tag
marker = "<!-- version-history:rows -->"
idx = lines.index(marker)
lines.insert(idx + 1, row)
open(page, "w", encoding="utf-8").write("\n".join(lines))
print(f"updated {page}: history row for {tag}" + ("" if is_beta else " + Latest Build block"))
PY
