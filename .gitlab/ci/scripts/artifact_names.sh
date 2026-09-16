#!/usr/bin/env bash
# The ONE place a release filename is spelled. Source this file; do not run it.
#
#   <product>-<version>-<os>-<arch>[-<variant>].<ext>
#
# product: athenaeum | perseus (lower-case)      version: no leading "v"
# os:      macos | windows | linux               arch:    x64 | arm64 — nothing else
#
# `amd64`, `x86_64` and `aarch64` are Debian's, Linux's and Apple's words for
# the same two things; they never appear in a filename we publish (they may
# still appear INSIDE a .deb's control file — that is Debian's business).
# Reads VERSION from the environment. Used by the deploy job (rename + alias),
# the GitLab Release links, verify_release.sh and the download-page generator.

_check_vocab() {
  case "$1" in athenaeum|perseus) ;; *) echo "ERROR: product must be athenaeum or perseus, got '$1'" >&2; return 1 ;; esac
  case "$2" in macos|windows|linux) ;; *) echo "ERROR: os must be macos, windows or linux, got '$2'" >&2; return 1 ;; esac
  case "$3" in x64|arm64) ;; *) echo "ERROR: arch must be x64 or arm64, got '$3'" >&2; return 1 ;; esac
}

# artifact_name PRODUCT OS ARCH EXT [VARIANT]
artifact_name() {
  local product="$1" os="$2" arch="$3" ext="$4" variant="${5:-}"
  _check_vocab "$product" "$os" "$arch" || return 1
  : "${VERSION:?VERSION must be set (no leading v)}"
  if [ -n "$variant" ]; then
    printf '%s-%s-%s-%s-%s.%s\n' "$product" "$VERSION" "$os" "$arch" "$variant" "$ext"
  else
    printf '%s-%s-%s-%s.%s\n' "$product" "$VERSION" "$os" "$arch" "$ext"
  fi
}

# alias_name PRODUCT OS ARCH EXT [VARIANT] — the version-less stable link.
alias_name() {
  local product="$1" os="$2" arch="$3" ext="$4" variant="${5:-}"
  _check_vocab "$product" "$os" "$arch" || return 1
  if [ -n "$variant" ]; then
    printf '%s-%s-%s-%s.%s\n' "$product" "$os" "$arch" "$variant" "$ext"
  else
    printf '%s-%s-%s.%s\n' "$product" "$os" "$arch" "$ext"
  fi
}

# all_release_artifacts — the inventory of every file a release publishes.
# One line each: product os arch ext variant subdir filename alias
# (variant is "-" when there is none). Order = the download page's order.
all_release_artifacts() {
  : "${VERSION:?VERSION must be set (no leading v)}"
  local spec product os arch ext variant subdir
  # product os arch ext variant subdir
  for spec in \
    "athenaeum windows x64 msi - windows" \
    "athenaeum windows x64 exe setup windows" \
    "athenaeum macos arm64 dmg - macos" \
    "athenaeum macos x64 dmg - macos" \
    "athenaeum linux x64 AppImage - linux" \
    "athenaeum linux x64 deb - linux" \
    "perseus macos arm64 dmg - perseus" \
    "perseus macos x64 dmg - perseus" \
    "perseus windows x64 exe setup perseus" \
    "perseus linux x64 deb - perseus" \
    "perseus linux x64 tar.gz - perseus" \
    "perseus linux arm64 deb - perseus" \
    "perseus linux arm64 tar.gz - perseus"; do
    read -r product os arch ext variant subdir <<< "$spec"
    local v="" ; [ "$variant" != "-" ] && v="$variant"
    printf '%s %s %s %s %s %s %s %s\n' "$product" "$os" "$arch" "$ext" "$variant" "$subdir" \
      "$(artifact_name "$product" "$os" "$arch" "$ext" "$v")" \
      "$(alias_name "$product" "$os" "$arch" "$ext" "$v")"
  done
}

# updater_artifacts — the files the in-app updater downloads, each published
# beside its `<filename>.sig`. One line each:
#   product os arch ext variant subdir filename plugin_key
# plugin_key is tauri-plugin-updater's manifest key ({os}-{arch}[-{installer}]).
# The two Windows installers get installer-specific keys and there is NO plain
# `windows-x86_64` key on purpose: the plugin falls back to the plain key for
# a build of unknown installer type, which must not be handed the NSIS exe.
# Not on the download page — these are not for people.
updater_artifacts() {
  : "${VERSION:?VERSION must be set (no leading v)}"
  local spec product os arch ext variant subdir key
  for spec in \
    "athenaeum macos arm64 app.tar.gz - macos darwin-aarch64" \
    "athenaeum macos x64 app.tar.gz - macos darwin-x86_64" \
    "athenaeum windows x64 exe setup windows windows-x86_64-nsis" \
    "athenaeum windows x64 msi - windows windows-x86_64-msi" \
    "athenaeum linux x64 AppImage - linux linux-x86_64"; do
    read -r product os arch ext variant subdir key <<< "$spec"
    local v="" ; [ "$variant" != "-" ] && v="$variant"
    printf '%s %s %s %s %s %s %s %s\n' "$product" "$os" "$arch" "$ext" "$variant" "$subdir" \
      "$(artifact_name "$product" "$os" "$arch" "$ext" "$v")" "$key"
  done
}

# legacy_alias_pairs — old alias spellings kept as extra symlinks until
# v0.7.0 so links published before the rename keep resolving.
# One line each: subdir legacy_alias current_filename
legacy_alias_pairs() {
  : "${VERSION:?VERSION must be set (no leading v)}"
  printf 'windows Athenaeum-windows-x64.msi %s\n'        "$(artifact_name athenaeum windows x64 msi)"
  printf 'windows Athenaeum-windows-x64-setup.exe %s\n'  "$(artifact_name athenaeum windows x64 exe setup)"
  printf 'macos Athenaeum-macos-aarch64.dmg %s\n'        "$(artifact_name athenaeum macos arm64 dmg)"
  printf 'macos Athenaeum-macos-x64.dmg %s\n'            "$(artifact_name athenaeum macos x64 dmg)"
  printf 'linux athenaeum-linux-x86_64.AppImage %s\n'    "$(artifact_name athenaeum linux x64 AppImage)"
  printf 'linux athenaeum-linux-amd64.deb %s\n'          "$(artifact_name athenaeum linux x64 deb)"
  printf 'perseus perseus-linux-amd64.deb %s\n'          "$(artifact_name perseus linux x64 deb)"
}
