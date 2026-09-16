*Athenaeum v0.6.4-beta.1: nothing inside the application changed — this one rebuilds how a release is built, verified and delivered.*

This is a beta, and an unusual one: it carries no changes to Athenaeum itself.
Every commit in it is about the machinery that produces a release. It exists so
that machinery gets exercised end to end on a build nobody depends on, rather
than on the next real one. If you are on the stable channel this release does
not reach you at all, and there is no reason to install it.

## What's New

- **A release is now verified before anyone hears about it.** Every installer,
  both Docker architectures and the release post itself are fetched back from
  their public addresses, and the macOS disk images are put through the same
  Gatekeeper check a new Mac performs, *before* the release page, the update
  feed and the announcements go out. Previously the announcement went first and
  the evidence, if any, came after. A build that fails any of those checks now
  never becomes the one the app offers you.
- **Every release keeps permanent download links.** Each version has its own
  folder that stays for good, so a link in a forum post or a bug report still
  resolves years later. The version-less *latest* and *beta* links still point
  at the current build, but they only move after the verification above has
  passed.

## Changes

- **One naming scheme for every download**, across all three operating systems:
  `athenaeum-0.6.4-beta.1-macos-arm64.dmg` is the product, the version, the
  system and the processor architecture, in that order. Architectures are
  spelled `x64` and `arm64` everywhere, replacing the mix of `amd64`, `x86_64`
  and `aarch64` that varied by platform. The previous link spellings keep
  working until v0.7.0, so nothing saved or scripted breaks today.
- **The download page and this post are generated from the release notes.**
  They used to be written by hand after the fact, which is why the site could
  describe a release slightly differently from the release. There is now one
  text, and the site is built from it.
- **macOS packages are notarized once each instead of twice.** The duplicate
  submission was pure waste, though a smaller waste than it looked from the
  outside: Apple's notary service answers in about half a minute, so both disk
  images together spend a little over a minute there. The release time that
  actually mattered was going somewhere else entirely, and that is fixed too.
