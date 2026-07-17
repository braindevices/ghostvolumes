#!/usr/bin/env bash
# Regenerates the table of contents in README.md and documents/design.md
# in place, via doctoc (https://github.com/thlorenz/doctoc). Safe to
# re-run any time headings change - doctoc locates its own
# <!-- START doctoc -->/<!-- END doctoc --> markers and updates in
# place rather than duplicating.
#
# doctoc is an npm package, so this script provisions Node via fnm
# (https://github.com/Schniz/fnm, itself a Rust tool - `cargo install
# fnm`) rather than requiring a system-wide Node install.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

if ! command -v fnm >/dev/null 2>&1; then
  echo "fnm not found - installing via cargo..." >&2
  cargo install fnm
fi

eval "$(fnm env)"
fnm install --lts
fnm use lts-latest

cd "$repo_root"
npx --yes doctoc README.md documents/design.md \
  --title '**Table of Contents**' \
  --minlevel 2 \
  --toc-location before \
  --github
