# Snapshot-prune — progress
Plan: [snapshot-prune.plan.md](snapshot-prune.plan.md)

## Step 1 — Test helper: BTRFS snapshot + read-only flag via ioctl
**Status**: done
**Date**: 2026-10-08
### What was done
- `test_support::snapshot(source, dest_parent, name, read_only)` (`BTRFS_IOC_SNAP_CREATE_V2`, 4096-byte `btrfs_ioctl_vol_args_v2`) and `set_read_only(subvol, bool)` (`BTRFS_IOC_SUBVOL_SETFLAGS`), via the `libc` crate.
- Tests: struct size = 4096; snapshot shares content but diverges independently (inode 256); read-only refuses writes with EROFS, toggles back and forth. All run unprivileged on the BTRFS scratch dir.
### Deviations from plan
- None.
### Issues found / fixed
- None.

## Step 2 — `[vcs.*]` config: parse and merge, with shipped defaults
**Status**: done
**Date**: 2026-10-08
### What was done
- New `src/vcs.rs`: `VcsEntry { dir, guard, health }`, `defaults()` (git with guard+health; hg, svn, jj, dvc dir-only), `load(config_dir)` = defaults + `vcs.toml` entries replacing by name, validated (dir one plain representable component; commands non-empty argv; unknown keys rejected).
- `filenames::VCS_CONFIG_FILE_NAME = "vcs.toml"`.
- Tests: defaults; user override replaces whole entry + addition; seven invalid inputs rejected.
### Deviations from plan
- Separate `vcs.toml` instead of a `roots.d` drop-in: `roots.d` files deserialize unknown top-level tables as roots (`#[serde(flatten)]`).
- Temporary `#![allow(dead_code)]` in vcs.rs until steps 3–6 use it (marked ponytail).
### Issues found / fixed
- None.

## Step 3 — `vcs-manifest`
**Status**: done
**Date**: 2026-10-08
### What was done
- `vcs::manifest(root, vcs)`: walks the whole tree (no symlink following), and for every entry named a configured VCS `dir` (dir or file) records it and everything below: `<path:?>\t<kind>\t<size>\t<mtime.ns>\t<inode>`, sorted by path. Symlinks recorded as `link:<target>`. Unreadable dirs outside VCS metadata → warning + skip; inside → error.
- CLI `ghostvolumes vcs-manifest <path>` (stdout; a closed pipe isn't a panic).
- Tests: deterministic; covers `.git` dirs and a `.git` file, not the work tree; work-tree edits/deletions don't change it; HEAD edit, ref rewrite via rename (new inode), refs deletion do; symlinks recorded not followed; a read-only snapshot yields the identical manifest as its source.
- Real repo: 33,582 entries walked in 40 ms.
### Deviations from plan
- Walks the whole snapshot rather than only registered projects: independent of prune's own scoping, and the cost is negligible.
### Issues found / fixed
- Fixed a test ordering bug (deleted refs before the rename case) and the output order (path first, so sorting is by path).

## Step 4 — `prune` walk + decisions read from the snapshot
**Status**: done
**Date**: 2026-10-08
### What was done
- New `src/prune.rs`: `subvolume_of(snapshot)` (Snapper layout `<subvol>/.snapshots/<n>/snapshot`), `projects_in` (registered roots under the subvolume, mapped into the snapshot, missing ones skipped), `candidates` (walk each project; a dir whose decision — read from the snapshot, memoised per file — resolves `+` is selected and not descended; VCS dirs, symlinks, ignored dirs (`.ghostvolumes-ignore` at the snapshot root and project root) and other registered projects never entered).
- CLI `ghostvolumes prune --dry-run <snapshot> [--subvolume]` (deletion lands in step 5).
- Tests (real BTRFS snapshots): Snapper layout parsing; `+` at any depth without descending, anchored `/build`; closer `-` overrides; VCS/ignored/symlinked dirs never entered; later live decision edits don't affect the snapshot; nested registered project uses its own boundary; unregistered/missing projects ignored. CLI test with garbage `roots.d` and `compiled.tsv` still works (they're never read).
### Deviations from plan
- Global `default-ignore` (in roots.d) is intentionally not used, per the 'never read roots.d' rule; VCS dirs are excluded via `[vcs.*]` instead.
### Issues found / fixed
- None.

## Step 5 — `prune` guards 1–4 + deletion + logging + idempotence
**Status**: done
**Date**: 2026-10-08
### What was done
- `prune::prune`: errors (exit 2) on a read-only snapshot (new `btrfs::is_read_only`, `BTRFS_IOC_SUBVOL_GETFLAGS`); otherwise, per candidate, `check` then `remove_dir_all`. Outcomes `Pruned{path,bytes}` / `Refused{path,reason}`.
- Guards: (1) `convert::check_contained` (now pub(crate)); (2) subtree scan refuses any configured VCS dir/file or decision file inside (and an unreadable subtree); (3) refuses a candidate containing another registered project; (4) the enclosing repo's `guard` command via new `vcs::run` (`GIT_OPTIONAL_LOCKS=0`, timeout via `wait-timeout`, stdout drained on a thread); refused if it prints anything, fails, or the repo's metadata resolves outside the snapshot (`vcs::metadata_inside`, follows `gitdir:` files; linked worktrees → unsupported).
- Deletion errors → refused (partial deletion kept; next run finishes it). CLI: `pruned:`/`refused:` lines, exit 0/1/2.
- Tests (real BTRFS snapshots + real git): deletes only in the snapshot, sizes reported, live untouched; VCS dir / hg store / decision file / nested project inside → refused, content kept; git guard refuses tracked `src`, prunes untracked `target`, and leaves `.git` manifest identical; linked worktree → refused as unsupported; read-only nested subvolume → refused, then a second run finishes; read-only snapshot → error.
### Deviations from plan
- Guard timeout fixed at 60 s for now (configurable later if needed).
- Exit-code mapping (0/1/2) is tested through unit-level outcomes, not a CLI test: integration tests can't create BTRFS snapshots (no lib target to share the helper); the CI end-to-end run (step 8) covers it.
### Issues found / fixed
- None.

## Step 6 — `vcs-health` with `--changes` and timeouts
**Status**: done
**Date**: 2026-10-08
### What was done
- `vcs::health(snapshot, subvolume, vcs, changed, timeout)`: finds repos by their VCS dir/file (not descending into metadata), runs each `health` command in the repo (via `vcs::run`: optional locks off, timeout). `Ok | Unchanged | Failed | TimedOut | Unsupported`.
- Change filter: each repo's real metadata path (`metadata_inside`) mapped back to the live subvolume; checked only if a changed path lies under it. `vcs::parse_snapper_status` is fail-safe: any unparseable line (incl. fragments from a filename with a newline) → `None` → every repo checked.
- Unsupported: metadata outside the snapshot (linked worktree), or `objects/info/alternates` present.
- CLI `ghostvolumes vcs-health <snap> [--changes FILE] [--subvolume] [--timeout 300]`; exit 0 ok, 1 failed, 2 only timeouts.
- Tests (real git in a read-only BTRFS snapshot): no list → checked; empty / work-tree-only list → unchanged; ref change → checked; manifest unchanged after health; deleted object → Failed; `sleep 5` with 200 ms → TimedOut; alternates + out-of-snapshot worktree → Unsupported; parser cases incl. newline-split filenames.
### Deviations from plan
- Change detection reads `snapper status -o` text (fail-safe), per the reviewed plan — no D-Bus.
### Issues found / fixed
- None.

## Step 6b — Milestone review fixes (QA QR-1..7, Security SR-1..5)
**Status**: done
**Date**: 2026-10-08
### What was done
- **QR-1/SR-1 (critical):** `projects_in` drops a project reached through any symlinked component below the snapshot root (`check_contained(project, snapshot)`); before each `remove_dir_all`, the canonical path must lie strictly inside the canonical snapshot. Test: an absolute symlink `a → <outside>/a` with `+ target` there — the outside data survives.
- **QR-2:** `repo_of` returns every VCS present at the nearest repo dir; all their guards/allows run. Test: DVC+git repo with tracked `data/` → refused.
- **QR-3:** `vcs::run` drains stderr on a thread too. Test: a command writing 300 KB to stderr and exiting 3 fails promptly, not as a timeout.
- **QR-4/QR-6:** every `prune` error exits 2; a missing/unreadable `project-roots.list` or no registered project under the subvolume is an error (exit 2). CLI test for the three cases.
- **QR-5:** `parse_snapper_status(text, subvolume)` → `None` (check all) if any path isn't under our subvolume spelling. Test incl. `/var/home` vs `/home`.
- **QR-7/SR-3:** git guard runs with `--literal-pathspecs`. Test: a tracked dir named `:(attr:zz)x` is refused.
- **SR-2 (design):** new optional per-VCS `allow` command (must exit 0); git default `check-ignore -q` — a `+` inside a git repo only prunes what the repo ignores, so committed decisions can't drop untracked user work. Test: untracked, non-ignored `notes/` → refused; ignored `target/` → pruned.
- **SR-4:** `run` strips inherited `GIT_DIR`, `GIT_WORK_TREE`, `GIT_INDEX_FILE`, `GIT_COMMON_DIR`, `GIT_OBJECT_DIRECTORY`, `GIT_ALTERNATE_OBJECT_DIRECTORIES`, `GIT_CEILING_DIRECTORIES`; sets `GIT_NO_LAZY_FETCH=1`; git defaults add `-c core.hooksPath=/dev/null -c safe.bareRepository=explicit`; the guard refuses repos with `objects/info/alternates` or `commondir`.
- **SR-5:** `metadata_inside` only honours `gitdir:` on the first line (like git).
### Deviations from plan
- `GIT_LITERAL_PATHSPECS=1` as an env var broke `git check-ignore` ("pathspec magic not supported"), so `--literal-pathspecs` is passed only to the `ls-files` guard.
- No test for the GIT_* env scrubbing (it would need `set_var` in the shared test process).
- QR-7's `.venv/src/<pkg>/.git` noise (pip `-e git+`) → documented in step 8; health `Unsupported` stays log-only.
### Issues found / fixed
- See above; all reproduced findings now have regression tests.

## Step 7 — `discover` lists `CACHEDIR.TAG` dirs and unregistered projects
**Status**: done
**Date**: 2026-10-08
### What was done
- `discover`: a directory with a valid `CACHEDIR.TAG` (exact signature line, read via `read_regular_file`) is treated like a watched name, so it's listed as an undecided suggestion — never acted on.
- New `discover::unregistered_note`: after the report, each suggestion not covered by a registered project gets a note that `prune` never prunes it, with a shell-quoted `ghostvolumes projects register` command (or an escaped mention if the path has control characters).
- Tests: valid tag → suggested as NotYetConverted; a fake tag → ignored; registered vs unregistered note incl. quoting.
### Deviations from plan
- None.
### Issues found / fixed
- None.

## Step 8 — `contrib/` script, units, login check, docs, CI snapper end-to-end
**Status**: done
**Date**: 2026-10-08
### What was done
- **8a** `contrib/ghostvolumes-snapshot` (args: snapper config, subvolume; flock, mktemp, recovery of `pending`, delete on prune crash, manifest `cmp`, fail-safe `snapper status` → `vcs-health --changes`, single `verify=` tag), `contrib/systemd/ghostvolumes-snapshot@.{service,timer}` (instance = config, `SUBVOLUME` via drop-in), `ghostvolumes-notify@.service` (journal crit + notify-send), `contrib/ghostvolumes-login-check`. All bundled in the binary: `ghostvolumes contrib [name]` (cargo install only installs the binary). CLI test: lists, prints byte-for-byte, unknown name fails.
- **8b** `tests/snapshot_script.rs` + `tests/support/fake-snapper` (Python; real BTRFS snapshots via the same ioctls; only on dummy subvolumes in the test scratch dir, HOME/XDG_* sandboxed): runs the REAL script — prune+lock+ok; unchanged run skips health; commit → checked; deleted object → `health-failed` exit 1; crashed run → `recovered`; tracked `+ src` → `refused` exit 0. Scratch dir empty afterwards.
- **8c** `documents/snapshot-prune.md` (setup, Snapper config, units, linger, run steps, verify table, restore, keep-forever, snbk rule, integrity jobs, vcs.toml, unsupported cases incl. `.venv` editable git installs), README pointer, `security.md` 'Snapshot pruning' section. The doc's vcs.toml example is validated with the real loader.
- **8d** CI: `snapshot-prune-e2e` job — release build on ubuntu-24.04, then `scripts/ci-snapper-e2e.sh` in a privileged openSUSE Tumbleweed container (pinned digest), asserting snapper ≥ 0.12, loop BTRFS, non-root user via ALLOW_USERS: prune, unchanged, commit, crash recovery. Lint job runs shellcheck on contrib + scripts.
### Deviations from plan
- Script takes `<config> <subvolume>` as arguments instead of parsing `snapper get-config` (unverifiable here).
- `snapshot-script` printer generalised to `ghostvolumes contrib [name]` (all bundled files).
- **Not run locally:** the CI container job, shellcheck (not installed), `systemd-analyze verify` (no system bus). First CI run is their verification.
### Issues found / fixed
- Fake-snapper: Python's `fcntl.ioctl` caps immutable args at 1024 bytes → mutable bytearray. Test cleanup made non-panicking.
- CI scenario: replaced `! cmd` and `a && b && c` checks (ignored by `set -e`) with explicit tests.

## Step 9 — Retire the shim
**Status**: done
**Date**: 2026-10-08
### What was done
- **9a** Removed `shim/preload.rs`, the cdylib build in `build.rs` (now vergen only), `src/intercept.rs`, `src/shellinit.rs`, `src/preload_guard.rs`, the `intercept`/`shell-init` commands and the LD_PRELOAD guard in `main`, `init`'s shim install/staleness/ownership helpers, `tests/shim_ld_preload.rs`, `tests/preload_guard_cli.rs`, the intercept CLI test. `init` removes a leftover `libghostvolumes_shim.so` (regular file only; test with a symlink). Help test asserts `prune`/`vcs-*` present and `intercept`/`shell-init` gone.
- **9b** `shim/*_core.rs` → `src/` (git mv; `include!` paths updated; `shim/` gone); stale comments and the `init` help text fixed; unused `SHIM_LOG_FILE_NAME` dropped. Loopback probe tests still compile `src/btrfs_core.rs` standalone.
- **9c** README rewritten around snapshot-prune; FAQ workflow rewritten, shim Q&As replaced by 'what happened to intercept'; `intercept.md` deleted; `security.md` threat model updated, shim rows removed, 'Retired' section; `files.md`, `decision-files.md`, `convert.md`, `project-roots.md` updated; `design.md` history banner; CHANGELOG 'Unreleased' entry.
- 363 tests pass (the drop from 431 is the removed shim tests); loopback ignored tests pass.
### Deviations from plan
- `convert` kept (owner ruling R1). `design.md` body left as history under a banner rather than rewritten.
- CHANGELOG was stale since 0.3.2; only an Unreleased entry added.
### Issues found / fixed
- None.

## Step 10 — Final QA + security review by the two agents, one debate round, fixes, report
**Status**: done
**Date**: 2026-10-08
### What was done
- Final QA + Security review of 6b–9; both found the crash-recovery deletion bug; all findings fixed in step 10a with tests.
- Final report: `ai-work/snapshot-prune-report.md`.
### Deviations from plan
- SF-2 (repo controls .gitignore) needs an owner decision; documented as residual risk meanwhile.
### Issues found / fixed
- See step 10a.

## Step 10a — Final review fixes (QA QF-1..7, Security SF-1..5)
**Status**: done
**Date**: 2026-10-08
### What was done
- **QF-1/SF-1 (both found it; QA reproduced: a fully pruned snapshot deleted):** a run that died after `modify --read-only` but before tagging left a read-only `pending` snapshot; the next run's `prune` exited 2 and the script deleted it. Now `prune` returns a distinct `ReadOnly` error → exit 3; recovery keeps such a snapshot and tags it `recovered`. e2e test `a_run_that_died_after_locking_keeps_its_snapshot` (fails on the old script).
- **QF-2:** script refuses (exit 1) before creating anything if `<subvolume>/.snapshots` doesn't exist (e.g. the unit's `SUBVOLUME` drop-in missing); args are `:?`-checked. e2e test.
- **SF-3:** temp files in a private `mktemp -d`.
- **SF-4:** snapper CSV parsed tolerantly (numeric rows only, `index()` match, quotes ignored); login check wrapped in a function (no variable clobbering), filters to `verify=` rows (milestones skipped), ignores a `pending` row younger than 30 min, strips control characters.
- **QF-6:** `vcs::run` error line truncated to 300 chars.
- **QF-7:** remaining stale shim comments fixed across src/.
- **QF-3 tests:** e2e `a_prune_error_deletes_its_snapshot_and_pages`, `a_held_lock_or_a_wrong_subvolume_creates_nothing`.
- **CI (owner suggestion):** the real-Snapper job now installs snapper 0.13.x from the **OBS repo for Ubuntu** (key-verified, `signed-by`) on the runner itself — real systemd/D-Bus, no privileged container; also tests the read-only-pending recovery with real snapper.
- **SF-2 residual** documented in security.md (repo controls `.gitignore` too) — design decision left to the owner.
### Deviations from plan
- CI moved from a Tumbleweed container to OBS packages on the runner (owner).
- Not added: `systemd-analyze verify` in CI (ExecStart paths don't exist on the runner, so it would fail without stubbing).
### Issues found / fixed
- See above.

## Step 11 — SF-2: warn on new `+` rules (plan §8)
**Status**: done
**Date**: 2026-10-08
### What was done
- `prune --since <prev snapshot>`: compares the effective `+` sets (decision files the walk consults, as `(dir relative to the snapshot, pattern)`) of this snapshot and the previous one (only read). Each new line is printed with what it matches (dirs, files, newest mtime, measured before deletion), appended to `$XDG_STATE_HOME/ghostvolumes/events.log` (`<RFC 3339>\t<snapshot>\t<line>`) and the exit code is 4. `--since ""` or a baseline gone since listing = no baseline, so every rule is reported. A `--since` of another subvolume → exit 2. Without `--since`, nothing changes.
- `decision::plus_patterns`, `pattern_matches` made public; `xdg::state_dir`; `filenames::EVENTS_LOG_FILE_NAME`; `prune::{rules, new_rules, describe, append_events}`. The walk was refactored into a `Walk` struct that collects rules alongside candidates. New dependency: `humantime` (timestamps).
- The script passes the newest tagged snapshot as `--since`; exit 4 → `verify=rules-changed` unless a failure or `health-timeout` wins; `prev` (for `snapper status`) accepts `ok` and `rules-changed`.
- The login check reports `events.log` while non-empty (checked first, so it also works with no tagged snapshot) and accepts `rules-changed`.
- Docs: snapshot-prune.md ("New prune rules", tag table), files.md, security.md (SF-2 row rewritten with the corrected wording), README, CHANGELOG. CI scenario extended.
- Tests: 7 unit tests in `prune` (added/edited rule, widening-only, no baseline / new project, rules only from consulted files, baseline untouched, `describe`, append-only log); 1 for the state dir; a CLI test (`--since` report, `""`, a gone baseline, a foreign subvolume → 2, dry run logs nothing); an e2e test (new rule → `rules-changed` + log line, earlier snapshot keeps `notes/`, next run `ok` with no new line, login check until deleted). Two e2e tests updated: the first run is now `rules-changed`. 376 pass; clippy clean; scratch dir empty.
### Deviations from plan
- `health-timeout` takes precedence over `rules-changed` (the plan had it the other way). Otherwise a snapshot whose health check timed out would become the next `snapper status` baseline and an unverified repo could be skipped. The warning is still in `events.log`.
- If a refused path and a new rule coincide, the tag is `rules-changed` (refusals stay in the journal), as planned.
### Issues found / fixed
- `--since ""` was first treated as "no flag" (no report); fixed to mean "no baseline".
- The login check returned early with no tagged snapshot, before reaching the events check; moved the check first.

## Step 11b — Step 11 review fixes (plan §8.1)
**Status**: done
**Date**: 2026-10-08
### What was done
- **Result comparison (owner: "compile the rules like in a real prune dry run then compare"):** `prune::newly_pruned` walks the snapshot twice: once with its own decision files, once with the baseline's (read at the same relative paths through a remapping `Files` reader). It reports paths pruned now that aren't in or under an old one. This replaces the Step 11 `+`-line set (`rules`/`new_rules`/`describe`/`plus_patterns` removed, `pattern_matches` private again). It covers a new `+`, a removed `-`, reordering and a removed nested override (S1/S6), with no build noise. A baseline file under a symlinked or non-directory ancestor counts as absent.
- **Log before deleting (QS-1):** `will prune (decisions changed): <rel> (N file(s), newest D)` is appended before `prune()` runs.
- **QS-2:** a failed append → stderr warning, pruning continues, still exit 4.
- **Read-only first:** a read-only snapshot exits 3 before any report, so a recovered, already-locked snapshot doesn't log false "will prune" lines.
- **Recovery:** passes `--since` with the newest usable snapshot below it (`base`: ok/refused/rules-changed/recovered/health-*/manifest-changed; never failed/pending), and exit 4 counts as success.
- **Never delete (owner):** a prune error locks the snapshot and tags it `verify=failed` (exit 1, alert), in the main run and in recovery. `tagged()` takes several tag prefixes.
- **S2 registration log:** `logging_registrations` in main.rs wraps `projects register`, `decide` and `convert`. Each project newly added to the list gets `registered: will prune <rel> (…)` lines, or `registered: no + paths yet`, in events.log. Library code has no XDG side effects.
- **`.ghostvolumes-ignore` dropped from prune (owner: "Let the convert respect it, not prune"):** the subvolume-root and project-root ignore files are no longer read. convert/decide are unchanged.
- **Owner question answered** ("all rules contained in the repo; no decision file means no prune"; does convert differ?): verified in code. prune resolves only through decision files from the project root down. convert discovers candidates via watched names, existing subvolumes, anchored `+`/`?` lines and `--create`, then resolves each through the same project decision files. Global config never adds a `+` in either.
- **Docs:**
  - snapshot-prune.md: a "Decision changes" section; the managed-subvolume rule; never-delete; tag table;
  - decision-files.md: project-only rules; `-` to keep a vendored path;
  - security.md, files.md, convert.md, README, CHANGELOG (breaking: prune ignores `.ghostvolumes-ignore`).
- **Tests:**
  - unit: every widening change reported; narrowing/cosmetic silent; nested override removed; new build output silent; no baseline; symlinked baseline ancestor; baseline only read; `summary`; prune ignores the ignore file and `-` keeps a path;
  - CLI: `--since` report, `""`, a missing baseline and a foreign subvolume; registration logs once;
  - e2e: QS-1 crash repro, QS-2 log-is-a-directory, prune error keeps the snapshot locked and `failed`; updated message expectations; CI scenario updated.
- 381 tests pass; clippy clean; the scratch dir is empty; nothing was written to the real `~/.local/state`.
### Deviations from plan
- S5 (refused + rules-changed both in the tag) is a docs note only, as both reviewers agreed after the debate.
- The upgrade notice for `.ghostvolumes-ignore` was dropped: the file stays legitimate for convert, so a per-run notice would be permanent noise. It's a CHANGELOG "Breaking" entry instead.
### Issues found / fixed
- The first draft would have logged "will prune" for an already-locked recovered snapshot; fixed with the read-only check before reporting.

## Step 11c — prune without project roots; `--since <time>` (plan §8.2, §8.3)
**Status**: done
**Date**: 2026-10-08
### What was done
- **Whole-snapshot walk:**
  - `prune` no longer reads `project-roots.list`; `projects_in`, the registered-project guard and the "nothing registered" exit 2 are removed.
  - The walk starts at the snapshot root, and `decision::resolve` is bounded by it, so a decision file applies to everything below it.
  - `check_contained` is against the snapshot.
  - `prune()` takes the candidate list `main` already computed: one survey of the current tree, plus the baseline survey.
- **`checked_snapshot`:** canonical `<subvolume>/.snapshots/<n>/snapshot`, and the snapshot root must be inode 256; otherwise exit 2. `--subvolume` is removed from `prune`.
- **Nested subvolumes, verified on real BTRFS:** in a snapshot a nested subvolume is an empty inode-2 directory with **its own `st_dev`**.
  - The walk skips such placeholders.
  - Any other directory on another device (a real subvolume, or a mount) refuses the whole run.
  - `scan` refuses a candidate containing one; placeholders inside a candidate are deleted with it.
- **`snapper.rs`:**
  - `snapper --jsonout --utc -c C list --columns number,date,description,userdata,read-only` and `get-config` (`SUBVOLUME`), via `vcs::run`, which now sets `LC_ALL=C` and caps output at 16 MiB;
  - parsing: only the C key, null userdata, snapshot 0 skipped, `%F %T` as UTC; malformed → error;
  - `parse_since`: a humantime duration, or `YYYY-MM-DD[ HH:MM[:SS]]` UTC;
  - `baseline()`: the oldest usable snapshot in [date(self) − window, …) below self, else the newest usable before. Usable = description `pruned`, read-only, `verify` in ok/refused/rules-changed/recovered/health-*/manifest-changed.
- **`main.rs` prune:**
  - `--config`/`--since` (since requires config); a future or out-of-range `--since`, or a bad config name → exit 2;
  - `baseline_snapshot` checks that the config's `SUBVOLUME` matches;
  - baseline unavailable → every path in the journal, one summary line in events.log;
  - no candidates → a stderr note, exit 0;
  - `logging_registrations` (11b) and `discover::unregistered_note` removed.
- **Script:**
  - `SINCE` (default `1d`), passed as `--config "$C" --since "$W"` in recovery and in the main run; `base()` removed.
  - **Found by the new e2e test:** a `vcs-manifest` failure right after `create` aborted the script under `set -e` and left a writable `pending` snapshot. It now goes down the prune-error path: lock, `failed`, alert.
- **Login check:** "N event(s) in events.log".
- **fake-snapper:** `--jsonout`/`--utc`, JSON `list`, `get-config`, stored date (`FAKE_SNAPPER_DATE`), description, read-only.
- **CI script:** no `projects register`; manual pending snapshots carry `--description pruned`; asserts no "baseline unavailable" (the real JSON was parsed and the config matched).
- **Docs:**
  - snapshot-prune.md: rules reach everything below; the real-snapshot rule; the window and repeats; SINCE; retention; who can suppress; manual review;
  - security.md (rows rewritten), FAQ, README, files.md, project-roots.md, CHANGELOG (breaking + "read events.log after upgrading").
- **Tests:**
  - unit (`snapper`): 10;
  - unit (`prune`): rules from above reach repos, nested `-` wins; placeholders skipped and deletable; a real subvolume below the root → error; only real snapshot paths; a symlinked dir never walked or deleted through; a live subvolume inside a candidate refused, then pruned once removed;
  - CLI: dry run with garbage config and no project list; exit 2 for the live tree, a plain layout dir and a symlink to the live tree; bad `--since`/`--config`; no snapper → summary line;
  - e2e: the window repeats, then stops (back-dated snapshots); no decision file → `ok`; the crash repro now shows the recovery line plus one in-window repeat; a broken vcs.toml → snapshot kept, `failed`.
- 394 tests pass; clippy clean; scratch dir empty; nothing written to the real `~/.local/state`.
### Deviations from plan
- A symlink in the `.snapshots/<n>/snapshot` position that points at another real snapshot is accepted: `absolutize` canonicalizes first, so it prunes that real snapshot, which is legitimate. One pointing at the live tree is refused.
- Not done: a separate "--since must be read-only" check. The baseline comes from the listing filtered on `read-only` true, and the path is built from the config's subvolume.
### Issues found / fixed
- The script's set -e abort after `create` (above).
- `SystemTime` before 1970 is representable, so a huge delta isn't an overflow; only `Duration::MAX`-class values are rejected.

## Step 11d — real snapper locally (owner request)
**Status**: done
**Date**: 2026-10-08
### What was done
- `scripts/dev-snapper.sh` (local dev, EL9 container, root):
  - downloads `snapper-0.13.0-3.el10_3.src.rpm` and checks its signature against the EPEL 10 key in a private rpm db;
  - adapts the spec for EL9 and rebuilds it with `rpmbuild`; caches the result in `target/dev-snapper/root`, so a re-run takes 0.01 s;
  - writes a `bin/snapper` wrapper: `--no-dbus --root "$DEV_SNAPPER_ROOT"`, with `get-config`'s SUBVOLUME and `status` paths made absolute again.
- **Spec changes for EL9:**
  - the `%conf` step moves into `%build` (rpm 4.16 has no `%conf`);
  - `--disable-rollback`, because rollback's default/active-subvolume queries need CAP_SYS_ADMIN, which this container lacks, so `list` failed;
  - `mksubvolume` is dropped from `%files`.
  - Snapper's own `make check` passes.
- **Installed system-wide (dnf, with the owner's OK):** rpm-build, dnf-plugins-core and the spec's BuildRequires. Nothing else: snapper is never installed, and `/etc/snapper` / `/etc/sysconfig/snapper` were never created. `create-config` writes /etc even with `--root`, so the tests hand-write the config under the scratch root.
- **Real output checked:** `--jsonout --utc list --columns number,date,description,userdata,read-only` matches `snapper.rs`'s assumptions exactly (config key; snapshot 0 with `""` date and `null` userdata; `"%F %T"`; userdata object; `read-only` bool, updated by `modify --read-only`). Also checked: `get-config` JSON, the `status -o` line format, and CSV.
- **`tests/snapshot_script.rs`:** with `GHOSTVOLUMES_DEV_SNAPPER=target/dev-snapper` it runs the real script against real snapper. 10/10 pass (the window test skips: no back-dating with real snapper). `verify_tag` uses `--csvout list --columns number,userdata` in both modes; the hourly test asserts that `info.xml` exists exactly when snapper is real.
- `tests/support/btrfs.rs` is shared by the integration tests.
- CI's ShellCheck covers the new script.
### Deviations from plan
- A mount namespace (to use real `/etc` paths privately) isn't permitted in this container, so `--root` plus the path-rewriting wrapper is used instead.
### Issues found / fixed
- Subvolume destroy (`SNAP_DESTROY`) is also EPERM here; cleanup uses `rmdir` on emptied subvolumes, as the tests already did.

## Step 11e — final review fixes (Steps 11b–11d review, QA + Security)
**Status**: done
**Date**: 2026-10-08
### What was done
Both reviewers found nothing Critical or High and agreed on one fix list after a rebuttal round:
1. **Q2 + L3 (script):**
   - A failed `vcs-manifest` no longer skips prune. The guards protect `.git`; the manifest only proves it afterwards. The run prunes, skips the comparison, still runs health, logs "pruning without a before-manifest" and tags `failed` (exit 1).
   - The lock and the provisional tag are one `snapper modify --read-only --userdata verify=pending|failed`, so a crash can't turn a failed run into `recovered`.
2. **L1:** the baseline path must pass `checked_snapshot` (canonical layout, subvolume root, same subvolume) and be read-only; otherwise "baseline unavailable".
3. **L4/Q3, pinned:**
   - the SRPM sha256 and the EPEL 10 key fingerprint (matches fedoraproject.org/security) in `dev-snapper.sh`;
   - the OBS key fingerprint (as first seen; the same for xUbuntu 22.04/24.04) in the CI script.
   All three were checked against the downloaded files.
4. **L5/Q4:** the wrapper is written from a quoted heredoc (only the checked `$OUT` is substituted). awk reads `DEV_SNAPPER_ROOT` from `ENVIRON`, so it's spacing-independent and its value is never code. Checked with a hostile root value (`/odd|e;#&\1` is copied literally).
5. **Q1:** `Env::drop` clears RO flags (`make_writable`, never through a symlink) on leftover snapshot subvolumes, so the TempDir rmtree removes them. Real mode now leaves 0 (counted with `ls -A`).
6. **Q5 tests (e2e, both modes):**
   - a failed before-manifest: still pruned, health ran, `failed`, exit 1, locked;
   - `manifest-changed`, via a `ghostvolumes` hook wrapper;
   - a non-snapshot baseline hides nothing.
   - `--since` without `--config` was already covered (CLI test).
7. **Docs:** snapshot-prune.md (steps 3/5, the `failed` row, the baseline-unavailable line per run), security.md (timer row, baseline check, supply chain), CHANGELOG.
- 397 tests pass; with `GHOSTVOLUMES_DEV_SNAPPER` the 13 e2e tests pass against real snapper; clippy clean; the scratch dir is empty (`ls -A`).
### Deviations from plan
- L2 (exit 5 for "changed + refused") was dropped by both reviewers: it's documented (the `rules-changed` row).
- Under real snapper, the non-snapshot baseline test sees snapper itself drop the writable plain dir (so no baseline, and the full list is logged), rather than prune rejecting it. Both outcomes are fail-safe, and the test accepts either.
### Issues found / fixed
- **Earlier "scratch dir empty" claims (Steps 11b–11d) were made with `ls` without `-A`, which hides the `.tmp*` dirs.** The default (fake) suite really did leave nothing; the real-snapper runs had left read-only snapshots. 34 leftovers were cleaned, and the root cause (real `snapper delete` needs SNAP_DESTROY, EPERM here) is fixed by item 5.

## Step 11f — fix the platform-gating CI job (macOS/Windows build)
**Status**: done
**Date**: 2026-10-08
### What was done
- `absolutize` in `src/main.rs` was not `#[cfg(target_os = "linux")]`, but its `PathBuf` import is, so the non-Linux fallback binary didn't compile (E0425). Gated it like its callers.
- Verified with `cargo check`/`clippy --target x86_64-pc-windows-gnu` and `x86_64-apple-darwin` (std targets added via rustup); Linux: fmt, clippy clean, 397 tests pass.
### Deviations from plan
None (not a planned step; a CI failure the owner reported).
### Issues found / fixed
- The break dates from audit-fixes Step 4 (b1e66e7), not from this branch's own steps.
- It went unnoticed locally because the documented wasm32 cross-check no longer gets that far: `wait-timeout` (added in Step 5) doesn't compile for wasm32. The windows-gnu/darwin `cargo check` is the reliable local check now.
- Owner follow-up ("gate the Linux-only dependencies"): all runtime `[dependencies]` moved to `[target.'cfg(target_os = "linux")'.dependencies]`, since the non-Linux fallback `main` only calls `eprintln!`. `Cargo.lock` unchanged; `cargo clippy --locked` is clean for wasm32-unknown-unknown, x86_64-pc-windows-gnu and x86_64-apple-darwin, so the wasm32 check works again. Linux: fmt, clippy clean, 397 tests pass.

## Step 11g — refuse to build off Linux (`compile_error!`)
**Status**: done
**Date**: 2026-10-08
### What was done
- Owner ruling: building anywhere but Linux makes no sense (no BTRFS). `src/main.rs` now starts with `#[cfg(not(target_os = "linux"))] compile_error!("GhostVolumes only supports Linux with BTRFS.");`; the 35 per-item `#[cfg(target_os = "linux")]`, the stub `main` and the Linux-only dependency table (Step 11f) are gone; `#[cfg(all(target_os = "linux", test))]` → `#[cfg(test)]`.
- The `platform-gating` CI job (macOS/Windows runners) is deleted. design.md (non-goals) and CHANGELOG updated; supersedes main plan §8.3's stub binary.
- `absolutize_tests` moved to the end of main.rs (clippy `items_after_test_module`, previously hidden by the cfg).
### Deviations from plan
None.
### Issues found / fixed
- Checked: macOS (`cargo build --target x86_64-apple-darwin`) prints the message first, then one more error (`libc::BTRFS_SUPER_MAGIC`); Windows (`cargo check`) prints it first, then ~30 more (`std::os::unix`). wasm32 now stops earlier, in `wait-timeout`, before our crate; wasm32 isn't a target anyone installs on. Linux: fmt, clippy clean, 397 tests pass.

## Step 11h — CI snapper e2e: clean environment for the `dev` user
**Status**: done
**Date**: 2026-10-08
### What was done
- First CI run of `snapshot-prune-e2e` (snapper 0.13.2 from OBS installed and the key pin passed) failed: `Error: /home/runner/.config/ghostvolumes/vcs.toml: Permission denied`, git warnings about `/home/runner/.config/git/*`, snapshot 1 `verify=failed`. `sudo -u dev -H` kept the runner's `XDG_CONFIG_HOME`; `-H` resets only `HOME`.
- `scripts/ci-snapper-e2e.sh` now runs the `dev` shell under `env -i` with only HOME/USER/LOGNAME/LANG/PATH.
- Verified locally by simulating the leak (`XDG_CONFIG_HOME=/home/runner/.config sudo -u nobody env -i …`): it's unset inside; `bash -n` ok. Confirmed by the owner's next CI run: `snapshot-prune-e2e` passes against snapper 0.13.2 (all assertions, including real `--jsonout` baseline lookup, recovery of a writable and of a read-only pending snapshot, and the repeated in-window `p/notes` warning on snapshots 5, 6 and 8 as designed).
### Deviations from plan
None.
### Issues found / fixed
- The pipeline behaved fail-safe: unreadable config → exit 2 → snapshot locked and tagged `failed`, the before-manifest failure was reported and pruning was not attempted silently.

## Step 11i — `snapper-interop` CI job no longer allowed to fail
**Status**: done
**Date**: 2026-10-08
### What was done
- Checked the owner's CI log archive (`gh-action-logs/logs_102401777394.zip`, `develop` at e001690): all six jobs (lint incl. shellcheck, msrv, test 24.04/26.04, snapshot-prune-e2e, snapper-interop) have no `##[error]`; tests 802 passed, 0 failed across both test jobs. `snapper-interop` passed every step against Ubuntu's snapper 0.10.6.
- Removed its `continue-on-error: true` and the comment that said to drop it once a real run passed.
### Deviations from plan
None.
### Issues found / fixed
None.

## Step 11j — read-only recovery prints a note, not prune's error
**Status**: done
**Date**: 2026-10-08
### What was done
- Owner request after the first green CI run: recovering a snapshot whose run died after locking printed prune's exit-3 error ("is read-only; prune needs a writable snapshot") to the journal, though it's the expected case. The recovery loop now holds prune's stderr in the private temp dir, shows it unless the exit is 3, and on 3 prints `note: snapshot N was already locked when its run died; kept as-is` (stdout).
- `a_run_that_died_after_locking_keeps_its_snapshot` asserts the note and the absence of the error (failed before the fix); the CI e2e greps for the note.
- Fake and real snapper (dev build 0.13.0): 13/13; full suite 397 pass, clippy clean, scratch dir empty.
### Deviations from plan
None.
### Issues found / fixed
- In recovery, prune's other stderr (refusals, warnings) now prints after its stdout instead of interleaved; acceptable for a leftover's recovery.
