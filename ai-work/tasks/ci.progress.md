# CI — Progress

Tracks implementation of `ai-work/tasks/ci.plan.md`. Each step: implement →
validate as much as this sandbox allows → commit → update this file → commit
this file.

**Environment note:** this sandbox can't run GitHub Actions itself, and
`snapper` isn't packaged for AlmaLinux/EL9 (checked — Debian/Ubuntu/openSUSE-
centric tool), so the `snapper-interop` job's exact command sequence can only
be validated by reasoning + the reference project's own precedent, not a real
local run — consistent with why it's marked `continue-on-error` in the plan.
YAML syntax is validated via `uv run --with pyyaml python3 -c "import yaml; ..."`
(no yamllint/actionlint available in this sandbox either); each `run:` block's
shell syntax is separately checked with `bash -n`.

Branch: `claude-ghostvolumes-impl`

---

## Step 1 — `.github/workflows/ci.yml`: `lint` job
**Status**: done
**Date**: 2026-07-10
### What was done
`fmt`/`clippy` job, `dtolnay/rust-toolchain@stable` with `rustfmt`/`clippy` components, `Swatinem/rust-cache@v2`. Verified locally: `cargo fmt --check` and `cargo clippy --all-targets -- -D warnings` both exit 0 against the current branch.
### Deviations from plan
None.
### Issues found / fixed
None.

## Step 2 — `.github/workflows/ci.yml`: `test` job (matrixed, real loop-mounted BTRFS)
**Status**: done
**Date**: 2026-07-10
### What was done
Matrixed `ubuntu-24.04` (required) / `ubuntu-26.04` (preview, `continue-on-error`), loop-mounted 2G `mkfs.btrfs` image via `sudo`, `chown`'d to the runner user, `GHOSTVOLUMES_TEST_BTRFS_DIR` pointed at it, then plain `cargo test` plus a bonus `cargo test --test btrfs_loopback -- --ignored --nocapture` step (first real chance for the `unshare`-based path to actually succeed, since this sandbox never could). Verified locally: `cargo test` (146 non-ignored + skip-guarded 4 ignored) all green on this sandbox's own real BTRFS.
### Deviations from plan
None.
### Issues found / fixed
**2026-07-10, post-first-real-run:** the first actual GitHub Actions run
of this job (before a GitHub remote existed locally, so unseen until the
user ran it and shared the log) hit a real, fully deterministic failure
on the `ubuntu-26.04` leg only: `debug_mode_logs_every_decision_with_its_reason`
failed a log assertion. Root cause (confirmed by reading both
implementations' actual source): Ubuntu 26.04 (devel) has switched its
default `coreutils` package to the Rust `uutils/coreutils`
reimplementation, whose `mkdir` calls `path.exists()` (a `stat()`, not
`mkdir()`/`mkdirat()`) before attempting creation and short-circuits
immediately when the target already exists — so this shim's intercepted
symbols are never entered for that case, unlike GNU coreutils (still
default on 24.04), which always calls `mkdir()` first unconditionally.
Not a race, not a concurrency issue (a `--test-threads=1` attempt along
the way didn't help, as expected once the real cause was found). Fixed
by making the test tolerate either `mkdir` implementation: a permanent
`-> ENTER` debug-log line (`shim/preload.rs`) now distinguishes "shim
entered but decided X" from "shim never entered," and the test asserts
the actual invariant (never a second subvolume, still a real subvolume
afterward) instead of assuming a specific libc call pattern. Also found
and fixed, along the way, an unrelated real bug: `shim/git_core.rs`'s
internal `git` invocation wasn't stripping `LD_PRELOAD`, causing it to
recursively (harmlessly, but wastefully) reload the shim into itself.
See `ai-work/tasks/ci-debug-log-test.plan.md` for the full investigation.

## Step 3 — `.github/workflows/ci.yml`: `snapper-interop` job
**Status**: done
**Date**: 2026-07-10
### What was done
Installs `btrfs-progs` + `snapper`, loop-mounts a fresh image, runs `sudo snapper --no-dbus -c ghostvolumes-test create-config -f btrfs <mount>`, asserts `.snapshots` exists with inode 256 (fails loudly right here, not confusingly two steps later, if some snapper version/flag behaves differently), then asserts `ghostvolumes scan` and `scan --save` (+ `compiled.tsv` non-empty) both detect it. `continue-on-error: true` since the exact command sequence is researched (verified via `snapper`'s manpage: `create-config`'s syntax, `--no-dbus` flag, and that it creates `.snapshots`) but not run for real anywhere yet — this sandbox has no `snapper` package at all (checked: not available for AlmaLinux/EL9, a Debian/Ubuntu/openSUSE-centric tool).
### Deviations from plan
None.
### Issues found / fixed
None found locally (couldn't run this job's core logic locally at all, per the environment note above) — genuinely first-real-run-dependent, flagged as such via `continue-on-error` and the plan's own step 5.

**2026-07-11, post-real-run:** `scan --save + reload round-trip` failed
silently (exit 1, zero output — `set -e` tripping on a `grep -q`/`test -s`
whose own semantics never print anything either way). Root cause:
this job never ran `ghostvolumes init` before `scan --save`, so no
`watched.d` names were configured at all; `cache::compile()` correctly
produces a *genuinely empty* `compiled.tsv` when there are zero watched
names, even with a real root present — `test -s` then (silently) fails.
Fixed by adding `./target/debug/ghostvolumes init` before `scan --save`
(seeds default watched names, matching real first-time usage), and by
replacing the silent `grep -q`/`test -s` checks with explicit,
existence-vs-content-distinguishing ones: each file is checked for
*existence* first (`::error::$file does not exist`, plus a directory
listing) and only then for *content* (`::error::$file exists but does
not contain ...` / `::error::$file exists but is empty (0 bytes)`), so
a future break says exactly which of the two happened instead of one
ambiguous "missing or empty" message. Verified locally: `ghostvolumes
init` + a manual `reload` against this sandbox's own real BTRFS root
produces a correctly non-empty `compiled.tsv`, and both diagnostic
branches (missing vs. empty) were manually triggered to confirm they
print the right message.

## Step 4 — `.github/workflows/ci.yml`: `platform-gating` job
**Status**: done
**Date**: 2026-07-10
### What was done
Matrixed `macos-latest`/`windows-latest`, `cargo build` then `cargo run --quiet` (not a hardcoded `./target/debug/ghostvolumes` path, to avoid needing to special-case Windows' `.exe` suffix — a robustness fix made during implementation, not in the original plan draft), asserting a nonzero exit code and the "only supports Linux with BTRFS" message.
### Deviations from plan
Switched from `cargo build` + invoking the binary directly to `cargo build` + `cargo run --quiet`, specifically to sidestep Windows' `.exe` extension without adding OS-conditional path logic. Same behavior, simpler/more portable invocation.
### Issues found / fixed
None.

## Step 5 — Push and confirm every job goes green on GitHub Actions
**Status**: blocked — this repo has no real GitHub remote configured yet (local-only so far)
**Date**:
### What was done
### Deviations from plan
### Issues found / fixed
Every job was validated as thoroughly as this sandbox allows short of an actual GitHub Actions run: YAML structure parses correctly (job names, `runs-on`, `continue-on-error`, `strategy.matrix`, step counts all confirmed via a script), every `run:` block passes `bash -n`, and every command this sandbox *can* exercise locally (fmt, clippy, `cargo test`, `cargo build`) does. The two things that genuinely can't be confirmed without a real GitHub Actions run: whether `snapper create-config`'s exact flags/behavior match what was researched, and whether the `unshare`-based `btrfs_loopback` bonus step actually succeeds (vs. gracefully skips) on GitHub's specific runner configuration — both already flagged via `continue-on-error`/informational framing rather than assumed to work.

## Step 6 — `ai-work/tasks/main-plan.md`: testing-conventions note
**Status**: done
**Date**: 2026-07-10
### What was done
Added §8.6 "Testing conventions," mirroring the reference project's `design.md` addition: the one-tier (no mocked/real split) testing approach, `btrfs_scratch_dir()`'s resolution order and `GHOSTVOLUMES_TEST_BTRFS_DIR` opt-in for local contributors, `tests/btrfs_loopback.rs`'s self-contained opt-in layer, and the fake-`$HOME` convention in subprocess-based tests. Added a build-order entry (§9 item 11) pointing at this plan.
### Deviations from plan
None.
### Issues found / fixed
None.

---

**Steps 1, 2, 3, 4, 6 complete.** Step 5 (confirm green on GitHub Actions) is blocked on this repository having an actual GitHub remote — nothing further to do here until that exists and a branch gets pushed.
