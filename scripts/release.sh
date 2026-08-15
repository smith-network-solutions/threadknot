#!/usr/bin/env bash
#
# Cut a Threadknot release: tag master, and dispatch the build workflow at that
# tag so the three runners publish installers people (and the app's own updater)
# can install.
#
# WHY THE TAG IS COMPUTED, NOT CHOSEN
# -----------------------------------
# The running app's version is `0.1.<commit count of origin/master>`, baked in by
# build.rs. The updater compares that number against the newest release's tag.
# They are therefore the same number by construction — a release tagged anything
# else is either invisible (lower, so every installed copy thinks it is already
# newer and never updates again) or a phantom (higher, so every copy offers an
# update that installs the version it already has). So the tag is derived here,
# from the commit being released, and never typed by hand.
#
# USAGE
#
#   scripts/release.sh            # tag origin/master's tip and build it
#   scripts/release.sh --dry-run  # say what it would do, touch nothing
#
# Needs `gh` (authenticated) and push rights on the tag namespace.

set -euo pipefail

cd "$(dirname "$0")/.."

DRY=0
[ "${1:-}" = "--dry-run" ] && DRY=1

say() { printf '\033[1m==>\033[0m %s\n' "$*"; }
die() { printf '\033[31merror:\033[0m %s\n' "$*" >&2; exit 1; }

command -v gh >/dev/null || die "the GitHub CLI (gh) is not installed."
gh auth status >/dev/null 2>&1 || die "gh is not authenticated. Run: gh auth login"

say "fetching origin"
git fetch --quiet origin master --tags

# Release the tip of master and nothing else. Tagging a local branch would ship
# a build whose version number claims to be master's.
target="$(git rev-parse origin/master)"
count="$(git rev-list --count origin/master)"
base="$(node -p 'require("./package.json").version' | cut -d. -f1,2)"
tag="v${base}.${count}"

say "master is at ${target:0:9} · ${count} commits · releasing as ${tag}"

if git rev-parse --verify --quiet "refs/tags/$tag" >/dev/null; then
  existing="$(git rev-parse "refs/tags/$tag^{commit}")"
  [ "$existing" = "$target" ] ||
    die "$tag already exists on ${existing:0:9}, which is not master's tip. Land a commit first."
  say "$tag already points at this commit; re-dispatching the build"
else
  say "tagging ${tag}"
  [ "$DRY" = 1 ] || {
    git tag -a "$tag" -m "Threadknot $tag" "$target"
    git push origin "refs/tags/$tag"
  }
fi

# Dispatched against the tag rather than triggered by pushing it: the workflow
# is workflow_dispatch-only on purpose (see build.yml), and `--ref` is what makes
# `github.ref` a tag ref so the release job runs and the version gets stamped
# into the bundle filenames.
say "dispatching build.yml at ${tag}"
if [ "$DRY" = 1 ]; then
  echo "  (dry run — nothing was tagged, pushed or dispatched)"
  exit 0
fi
gh workflow run build.yml --ref "$tag"

say "running. watch it with:"
echo "  gh run watch \$(gh run list --workflow=build.yml -L1 --json databaseId -q '.[0].databaseId')"
echo
say "when it finishes, check the release page:"
echo "  gh release view $tag"
echo
echo "Every installed Threadknot picks this up within 30 minutes, or immediately"
echo "via Settings -> Updates -> check now."
