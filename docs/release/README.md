# Release


## Detail moved from CLAUDE.md (2026-10-20) - Release workflow and branching

### Release workflow

The procedure is the `release` skill (`.claude/skills/release/SKILL.md`): notes
(`RELEASE_NOTES.md` is the blog post), `scripts/release/bump.sh`, a release
commit that holds only notes + the six version files + `Cargo.lock`, push, green
GitHub run, tag. The tag pipeline (`.gitlab-ci.yml`) runs
`gate → build → deploy → publish → verify → announce`: it refuses a tag whose
versions or GitHub checks are not green, uploads to `artfrom.space/builds/<tag>/`
under the one naming scheme (`.gitlab/ci/scripts/artifact_names.sh`), publishes
Docker Hub and the docs site (post + download page generated from the notes),
fetches all of it back, and only then creates the GitLab Release, `version.json`
and the chat posts. Nothing is done by hand after the tag.

**Branching.** `main` is the development trunk and releases are tags on it. This
replaces the older "develop on a branch named after the version, ff-merge at
release" rule, which left `main` hundreds of commits stale — unworkable once
`main` is the default branch outside contributors base their pull requests on.
A release branch is cut only if a backport is ever actually needed.

