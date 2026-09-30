#!/bin/sh
# prev_tag.sh - the release-notes compare base (previous_tag_name) for a cut.
#
# WHY THIS EXISTS
#   GitHub anchors auto-generated release notes ("Full Changelog") on the most
#   recently PUBLISHED release. Since the daily pack-rust-vulns-* corpus-pack
#   releases started, that anchor is usually a corpus pack, not the previous
#   engine release: v1.0.0-rc.78 shipped comparing
#   pack-rust-vulns-2026-09-29...v1.0.0-rc.78 and had to be hand-patched via
#   REST. PR #1069 stopped trusting that auto-detection and pinned the base
#   with `git tag --sort=-v:refname`, but plain version sort orders a final
#   BELOW its own pre-releases (v1.0.0-rc.79 > v1.0.0), so the first v1.0.1
#   cut would have compared against v1.0.0-rc.79 rather than v1.0.0 — the
#   opposite of what #1069's message claims. `-c versionsort.suffix=-rc`
#   orders a prerelease below its final (git >= 2.36; GitHub runners ship
#   newer, and the fixture below fails loudly on a git that does not honor
#   it). This script is the single derivation, checked in with its own
#   fixture test so the ordering is provable without cutting a release.
#
# RULES
#   - Candidates match ^v[0-9]+\.[0-9]+\.[0-9]+ — an rc suffix counts, an rc
#     IS a real xerj release. pack-*, backup/* and rb* tags never match.
#   - The answer is the version-successor of the tag being cut: the next
#     entry below it in descending version order. A fresh cut therefore gets
#     the previous release, and re-dispatching an OLD tag rebuilds notes
#     against that release's own predecessor instead of today's newest tag.
#     A current tag not in the list yet (a hypothetical) takes the highest
#     entry.
#   - Nothing below it (first-ever tag): empty output, exit 0 — the workflow
#     falls back to GitHub's auto-generated notes.
#
# USAGE
#   bash .github/scripts/prev_tag.sh <tag-being-cut>   # prints the base, or nothing
#   bash .github/scripts/prev_tag.sh --self-test       # fixture assertions
#
# Requires: git >= 2.36 (versionsort.suffix), run in a checkout that has the
# tags (the release job checks out with fetch-depth: 0).

set -eu

# All release tags, highest version first.
vtags() {
  git -c versionsort.suffix=-rc tag -l 'v[0-9]*' --sort=-v:refname \
    | grep -E '^v[0-9]+\.[0-9]+\.[0-9]+'
}

# $1 = the tag being cut. Version-successor semantics, see header. awk
# detail: `exit` still runs END, so END must re-check `have` before
# applying the not-in-list fallback.
prev_tag() {
  vtags | awk -v cur="$1" '
    NR == 1   { head = $0 }
    $0 == cur { have = 1; next }
    have      { print; exit }
    END       { if (!have && head != "") print head }
  '
}

case "${1:-}" in
  --self-test) ;;
  '')
    printf 'usage: prev_tag.sh <tag-being-cut> | --self-test\n' >&2
    exit 2 ;;
  *)
    prev_tag "$1"
    exit 0 ;;
esac

# ── self-test: fixture repos whose tag CREATION order is not their version
#    order, so the assertions below fail for a creatordate sort (the pack
#    poison) and for plain --sort=-v:refname (final below its rcs). ─────────
fails=0
note() { printf '  %s\n' "$1"; }
fail() { printf 'FAIL  %s\n' "$1"; fails=$((fails + 1)); }
pass() { printf 'ok    %s\n' "$1"; }

# fixture <tag>... — fresh repo, one empty commit, tags created in the order
# given (identity is inline: a runner's global git identity does not apply to
# a repo that actions/checkout did not create).
fixture() {
  fx="$(mktemp -d)"
  git -C "$fx" init -q 2>/dev/null
  git -C "$fx" -c user.email=selftest@xerj.org -c user.name=selftest \
    commit -q --allow-empty -m fixture
  for t in "$@"; do git -C "$fx" tag "$t"; done
  printf '%s' "$fx"
}

# check <repo> <current-tag> <expected|-> <description>
check() {
  got="$(cd "$1" && prev_tag "$2")"
  if [ "${got:--}" = "${3:--}" ]; then
    pass "$4"
  else
    fail "$4: expected '${3:-<empty>}', got '${got:-<empty>}'"
  fi
}

echo '== fixture 1: rc chain with newer-created pack/rc.9 noise (todays shape) =='
f1="$(fixture v0.9.0 v1.0.0-rc.8 v1.0.0-rc.78 v1.0.0-rc.77 v1.0.0-rc.9 \
                pack-rust-vulns-2026-09-30 backup/old rb12)"
check "$f1" v1.0.0-rc.79 v1.0.0-rc.78 \
  'hypothetical cut today: previous v-tag, not pack-rust-vulns-2026-09-30 (created later) nor v1.0.0-rc.9'
check "$f1" v1.0.0-rc.78 v1.0.0-rc.77 \
  're-dispatch of the newest tag: its own predecessor'
rm -rf "$f1"

echo '== fixture 2: final among its rcs (the PR #1069 latent ordering bug) =='
f2="$(fixture v1.0.0-rc.8 v1.0.0-rc.79 v1.0.0 v1.0.1)"
check "$f2" v1.0.1 v1.0.0 \
  'v1.0.1 cut compares against v1.0.0, not v1.0.0-rc.79 (plain --sort=-v:refname picks the rc)'
check "$f2" v1.0.0 v1.0.0-rc.79 \
  'rebuilding the v1.0.0 release compares against v1.0.0-rc.79, not the newer v1.0.1'
rm -rf "$f2"

echo '== fixture 3: first-ever tag =='
f3="$(fixture v0.1.0)"
check "$f3" v0.1.0 - \
  'no predecessor: empty output, workflow falls back to auto-generated notes'
rm -rf "$f3"

if [ "$fails" -eq 0 ]; then
  printf 'PASS  prev_tag.sh: 5/5 fixture assertions\n'
else
  printf 'FAIL  prev_tag.sh: %d fixture assertion(s)\n' "$fails"
  exit 1
fi
