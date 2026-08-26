_A short follow-up to 0.5.1: masters rebuild from the library itself, and the duplicate path rule can be read backwards._

## What's New

- **Rebuild a master from its provenance block.** A master calibration file that Athenaeum built can now be re-integrated in place from the calibration library — the same file, the same catalog entry, refreshed from its original frames. The button appears only on masters Athenaeum built itself, since an imported one carries no recipe to replay, and it waits with an explanation while the source frames sit in an archive rather than on disk. Progress, the completion notification and the list refresh behave exactly as they do for a first build.
- **The duplicate path rule can be inverted.** "Path contains" marks the copies whose path contains one of your substrings. A new NOT checkbox flips it: the rule then marks the copies that match none of them, keeping only the ones that do — useful when you can name the copy worth keeping but not the strays worth losing. The rule's title, its description and the collapsed chain summary all follow the switch, so the panel never describes the opposite of what it is about to do. An empty pattern list leaves the rule idle in either direction.

## Changes

- **Version numbers drop the beta suffix.** Releases are plain `X.Y.Z` from here on, and this is the first of them. Nothing changes about how updates reach you.
- Internal cleanup: unreachable variants removed from the export, merge and operation-step models, and a formatting pass over the core API layer.
