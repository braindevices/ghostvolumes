#!/usr/bin/env bash
# Re-pins SHA-pinned GitHub Actions to the current commit of the ref
# named in their trailing comment:
#
#   uses: owner/repo@<40-hex sha> # <ref>
#
# <ref> is a tag or branch (e.g. `v2`, `master`). Lines without that
# exact shape (first-party `@v7` tags, local `./` actions) are left
# alone. Needs only git, sed and grep - no GitHub token.
#
# A SHA pin is only as good as its review: this pins whatever <ref>
# points to *now*. Before committing a bump, read the upstream diff
# (https://github.com/<repo>/compare/<old>...<new>); never auto-merge it.
# See documents/security.md ("Pinned GitHub Actions").
#
# Usage: scripts/bump-action-pins.sh [--check] [workflow.yml...]
#   --check  change nothing; exit 1 if any pin is out of date
# Default files: .github/workflows/*.yml
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

check=false
if [[ "${1:-}" == "--check" ]]; then
  check=true
  shift
fi
if [[ $# -eq 0 ]]; then
  set -- "$repo_root"/.github/workflows/*.yml
fi

# Commit sha for <ref> in github.com/<repo>: a tag's peeled commit
# (annotated tags point at a tag object), else the tag, else a branch.
# Tags win over a same-named branch: if upstream ever adds a tag called
# e.g. `stable`, that tag (not the branch) gets pinned.
resolve() {
  local repo="$1" ref="$2" out sha
  out="$(git ls-remote "https://github.com/$repo" \
    "refs/tags/$ref^{}" "refs/tags/$ref" "refs/heads/$ref")"
  for suffix in "refs/tags/$ref^{}" "refs/tags/$ref" "refs/heads/$ref"; do
    sha="$(awk -v r="$suffix" '$2 == r { print $1; exit }' <<<"$out")"
    if [[ -n "$sha" ]]; then
      echo "$sha"
      return
    fi
  done
  echo "error: $repo has no tag or branch named $ref" >&2
  return 1
}

pin_re='uses:[[:space:]]*([A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+)(/[^@[:space:]]*)?@([0-9a-f]{40})[[:space:]]*#[[:space:]]*([^[:space:]]+)'
stale=0
for file in "$@"; do
  while IFS= read -r line; do
    [[ "$line" =~ $pin_re ]] || continue
    repo="${BASH_REMATCH[1]}" old="${BASH_REMATCH[3]}" ref="${BASH_REMATCH[4]}"
    new="$(resolve "$repo" "$ref")"
    [[ "$new" == "$old" ]] && continue
    stale=1
    echo "$file: $repo ($ref): $old -> $new"
    if ! $check; then
      sed -i "s|$repo\([^@[:space:]]*\)@$old|$repo\1@$new|g" "$file"
    fi
  done < <(grep -E "$pin_re" "$file" | sort -u)
done

if $check && [[ $stale -ne 0 ]]; then
  exit 1
fi
