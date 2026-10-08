# Snapshot-prune — final report

**Date**: 2026-10-08 · **Branch**: `claude-snapshot-prune` (from `claude-shim-edition-2024`). 18 implementation commits after the plan; not pushed or merged.

| Document | Purpose |
|---|---|
| [`tasks/snapshot-prune.plan.md`](tasks/snapshot-prune.plan.md) | Plan: decisions, debate record, steps |
| [`tasks/snapshot-prune.progress.md`](tasks/snapshot-prune.progress.md) | Per-step record (done, deviations, issues) |
| [`../documents/snapshot-prune.md`](../documents/snapshot-prune.md) | User guide |

## 1. What changed

GhostVolumes no longer injects an `LD_PRELOAD` shim into build processes. Instead:
- Snapper takes a **writable** snapshot of the projects subvolume.
- `ghostvolumes prune` deletes the explicitly decided (`+`) volatile directories **inside that snapshot**.
- The snapshot is locked read-only, and Snapper's timeline cleanup keeps it.
- Nothing runs inside your tools, and your working tree is never touched.

| New | Role |
|---|---|
| `prune <snapshot>` | Walks registered projects inside the snapshot and reads decisions from the snapshot. Guards: containment, no symlinked ancestor, no VCS dir / decision file / nested project inside, every VCS present at the repo consulted, git tracked → refuse, git not-ignored → refuse, canonical path inside the snapshot right before deleting. Exit 0 clean, 1 refused, 2 error, 3 already read-only |
| `vcs-manifest <path>` | A byte-for-byte manifest of every VCS metadata dir; the script `cmp`s it before and after the prune |
| `vcs-health <snapshot> [--changes FILE]` | Runs each repo's health command (git: `fsck --connectivity-only`). Only for repos with changes under their metadata, per Snapper's own comparison (`snapper status`, a send-stream diff). Parsed fail-safe |
| `vcs.toml` | Per-VCS `dir` / `guard` / `allow` / `health`. git ships with commands; hg, svn, jj and dvc ship with only their dir |
| `contrib` | Prints the bundled timer script (~65 lines of sh), systemd units and login check |
| `discover` | Also suggests `CACHEDIR.TAG` dirs and flags unregistered projects |

**Retired:**
- `intercept`, `shell-init`, `preload_guard`;
- the build-time cdylib;
- the shim install (`init` now removes a leftover shim).

`shim/*_core.rs` moved into `src/`. `convert` stays (your ruling R1).

## 2. Verification

- **Tests:** 366 pass, 0 fail, 4 ignored (loopback, opt-in; those pass too).
- **Fake-Snapper end-to-end tests** (`tests/snapshot_script.rs`): the real script against `tests/support/fake-snapper`, which makes real BTRFS snapshots with the same ioctls. Covered:
  - prune and lock;
  - an unchanged run skips health;
  - a commit gets re-checked;
  - a missing object → `health-failed`, exit 1;
  - a crashed run (writable) → `recovered`;
  - a crashed run (read-only) → kept, `recovered`;
  - a prune error → snapshot deleted, exit 1;
  - a held lock / wrong subvolume → nothing created;
  - a tracked `+` → `refused`, exit 0.
- **Every test uses dummy subvolumes** in the test scratch dir, with HOME and XDG sandboxed. The scratch dir is empty after each run.
- **Not run locally** (first CI run verifies):
  - the real-Snapper job (`scripts/ci-snapper-e2e.sh`): Snapper 0.13.x from the **OBS repo for Ubuntu** (your suggestion), key-verified, on the runner itself;
  - ShellCheck;
  - `systemd-analyze` for the units.

## 3. Reviews

**Milestone (Steps 1–6) — QA + Security:**
- **Critical, reproduced:** prune could delete outside the snapshot via a symlinked project ancestor.
- High: DVC+git skipped git's guard.
- High: a stderr flood showed up as a timeout.
- Medium: exit-code collisions, a silent no-op, path spelling mismatches.
- Low: pathspec magic, inherited `GIT_DIR`, hooks/lazy-fetch hardening.

All fixed with regression tests (Step 6b).

**Final (Steps 6b–9) — QA + Security:**
- **Both found the same bug:** crash recovery could **delete a good, already-locked snapshot** (QA reproduced it).
- Others: an unset subvolume, a guessable temp file, CSV parsing, login-check issues, truncation of failure messages, stale comments, CI `snapperd` activation.

All fixed with tests (Step 10a).

## 4. SF-2 — decided and implemented (Steps 11, 11b)
- **Owner rulings:**
  - no conflict check, and the watch list stays a suggestion;
  - warn about decision changes, nothing more;
  - rules are per snapshot (never re-prune);
  - a plain log file;
  - compare dry-run results;
  - never delete snapshots;
  - `.ghostvolumes-ignore` belongs to convert, not prune.
- **Implementation:**
  - `prune --since <previous snapshot>` evaluates both snapshots' decision files on the same tree and reports what only the new ones prune. It's appended to `~/.local/state/ghostvolumes/events.log` before deleting, and the run exits 4 → `verify=rules-changed`.
  - Registering a project logs what it will prune.
  - Crash recovery compares too.
  - A failed prune keeps the snapshot (locked, `verify=failed`).
- **Reviews:**
  - Step 11: QA and Security found QS-1 (a crash lost the warning) and QS-2 (an unwritable log deleted the snapshot), both reproduced, plus S1/S2. All fixed in 11b, with tests.
  - Details: plan §8/§8.1, progress Steps 11/11b, security.md.
- **Step 11c (owner rulings):**
  - Prune has no project roots: the whole snapshot is managed, and decision files resolve up to its root.
  - Only a real snapshot is pruned; a real subvolume or mount inside it refuses the run.
  - `--since` is a time window (default `1d`), with the baseline picked in Rust from `snapper --jsonout list`. Repeated warnings within the window are accepted.
  - Reviewed by both agents before implementation (plan §8.2/§8.3).

## 5. Open / known
- Only registered projects are pruned.
- Unsupported (skipped with a message): git alternates, linked worktrees, symlinked VCS dirs.
- A `.venv` with `pip install -e git+…` is refused on every run (it contains a `.git`). Documented.
- No `systemd-analyze verify` in CI: the units' `ExecStart` paths don't exist on the runner.
- The `*_core.rs` + `include!` split survives from the shim era. It's harmless; folding it in is a possible later cleanup.
- `compiled.tsv` removal is a later, separate plan (decided earlier).
