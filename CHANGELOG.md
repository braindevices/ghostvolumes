# Changelog

Notable changes to this project, loosely following [Keep a Changelog](https://keepachangelog.com/).

## Unreleased

**Breaking: the `LD_PRELOAD` shim is retired; GhostVolumes now prunes Snapper snapshots instead.** See [snapshot-prune.md](documents/snapshot-prune.md).

- **Added** `prune` (delete decided volatile dirs inside a writable Snapper snapshot, with guards against escaping the snapshot, VCS metadata, tracked files, and — in git repos — anything not ignored), `vcs-manifest` (before/after proof that VCS metadata is untouched), `vcs-health` (per-VCS health command, only for repos Snapper reports as changed), `contrib` (prints the bundled snapshot script, systemd units and login check), and `vcs.toml` per-VCS settings.
- **Added** `prune --config <c> --since <1d|UTC time>`: compares with the oldest snapshot this tool pruned within the window before the snapshot's own date (from `snapper --jsonout list`), and reports what the snapshot's decisions prune that the earlier snapshot's decisions wouldn't (same tree, so any widening change counts: new `+`, removed `-`, reordering, a new repo); appends it to `~/.local/state/ghostvolumes/events.log` before deleting (reported by the login check until deleted) and exits 4; the timer script (`SINCE`, default `1d`) tags such snapshots `verify=rules-changed`. A change is reported by every run in the window. Pruning still applies them, and each snapshot is pruned only by its own decision files. New dependency: `serde_json`.
- **Changed** the timer script never deletes a snapshot: one `prune` can't handle is locked and tagged `verify=failed` (alerts), in one `snapper modify` with the lock. A failed `vcs-manifest` no longer leaves the snapshot unpruned: it prunes, runs health and tags `failed`. The CI pins the OBS key fingerprint.
- **Breaking:** `prune` no longer reads `.ghostvolumes-ignore` (`convert`/`decide` still do) or `project-roots.list`: the whole managed subvolume is pruned by its decision files alone, each applying to everything below it (a `~/src/.ghostvolumes-decisions` reaches every repo). Use a `-` decision to keep a path. **After upgrading, read `events.log`:** the first run lists everything now pruned, including directories with decision files that were never registered and parent rules reaching child repos.
- **Changed** `prune` only accepts a real snapshot (`<subvolume>/.snapshots/<n>/snapshot`, a subvolume root; `--subvolume` is gone) and refuses a run with a real subvolume or another filesystem inside it.
- **Added** `discover` suggestions for `CACHEDIR.TAG` dirs.
- **Removed** `intercept`, `shell-init`, the build-time shim compile and the shim install; `init` deletes a leftover `libghostvolumes_shim.so`.
- **Changed** `convert` keeps a reflinked, git-ignored backup by default (`--delete-backup` / `delete-convert-backup = true` to delete); `rust-version` is 1.95; many audit fixes (path escapes, symlinked decision files, parser panics, line-format injection) — see `ai-work/audit-2026-10-06.md` and `documents/security.md`.

## 0.3.2 — 2026-07-16

- **Added** a branch-based SemVer pre-release suffix to `ghostvolumes --version`, on top of 0.3.1's `git describe` output — this project's GitFlow-shaped branches (`.github/workflows/ci.yml`: `main` = release, `develop` = pre-release, plus `hotfix/*`/`feature/*`) map onto `-alpha` (`develop`), `-rc` (`hotfix/*`), `-dev` (anything else), or no suffix at all (`main`/`master`/detached HEAD). `git describe`'s own "commits past the last tag" count alone can't distinguish which branch a build came from — e.g. `0.3.2-alpha (v0.3.1-3-gabc1234)` on `develop` vs. `0.3.2 (v0.3.1)` on `main`. Computed independently in `build.rs` via its own `git rev-parse --abbrev-ref HEAD` call, since `vergen-gitcl`'s own branch detection only surfaces as a `cargo:rustc-env` var for the *final crate*, not readable back mid-build-script.

## 0.3.1 — 2026-07-16

- **Added** `git describe` output to `ghostvolumes --version` (e.g. `0.3.1 (v0.3.1)`, or `0.3.1 (v0.3.1-3-gabc1234)` a few commits past a tag) via `vergen-gitcl` in `build.rs` — `CARGO_PKG_VERSION` alone can't distinguish two builds that both claim the same version but come from different commits. Chose the `git`-CLI-shelling-out backend over `vergen-gix`/`vergen-git2`: `git` is already a hard prerequisite for this project's only supported install path (`cargo install --git`), so shelling out to it costs nothing extra, unlike `vergen-gix`'s ~500-crate transitive dependency tree or `vergen-git2`'s libgit2 C dependency.
- Tagged `v0.3.0` (retroactively, at the commit the 0.3.0 release actually landed on) and `v0.3.1` — this project didn't tag releases before now.

## 0.3.0 — 2026-07-13

Cross-process atomic file I/O, plus a proper project-roots lifecycle. See [design.md](documents/design.md) for the full rationale.

- **Changed** `atomic_write.rs`'s temp filenames to include the writing process's PID and a per-process counter, closing a corruption window where two concurrent writers to the same destination shared one temp path.
- **Changed** every append-based writer (`register`'s append, `convert`'s decision-file append, `discover --save`, the shim's log line) from multi-piece `writeln!` to a single `write_all()` call per line, so a concurrent appender can never land mid-line.
- **Added** `std::fs::File`-based advisory locking (`reload.lock`, `project-roots.lock`, and a per-project-boundary lock) fully serializing `reload`/`scan --save`, `projects register`/`unregister`, and — the one genuinely dangerous race — the shim's subvolume creation against `convert`'s directory swap for the same project.
- **Renamed** `project-roots.txt` to `project-roots.list` — not because it's disposable (it's persistent user data with no other source of truth, safe to back up or sync via a dotfile manager), but because `.txt` invited hand-editing that raced the shim's and CLI's own atomic access to it.
- **Added** `ghostvolumes projects list`/`register`/`unregister`, replacing the flat `ghostvolumes register <path>` command. `unregister` (no path) scans every registered root and interactively offers to prune ones that no longer exist — including entries that arrived already-stale via a synced/copied-in project-roots list.
- **Fixed** `convert`'s `create_empty` to tolerate the shim having already created the target subvolume in a race, matching the shim's own tolerance for the same case.

## 0.2.0 — 2026-07-13

Decision-model rewrite: replaces the git-tracked gate and shell `cd`-hook with an explicit, gitignore-style decision-file model. See [design.md](documents/design.md) for the full rationale.

- **Removed** the git-tracked gate (`is_git_tracked`, shelling out to `git ls-files`) and all VCS-based detection.
- **Removed** the proactive `cd`-hook / shell-integration activation path (`ghostvolumes ensure`, `shell-init`'s hook mode). `ghostvolumes intercept -- <cmd>` is now the sole activation path; `shell-init` prints a diagnostic value only.
- **Added** per-directory `.ghostvolumes-decisions` files (`+`/`-` gitignore-style patterns), resolved live by the shim on every intercepted call.
- **Added** `ghostvolumes convert <path>` for explicit, interactive conversion and decision recording.
- **Added** `ghostvolumes register <path>` and the project-roots list, narrowing decision-file lookups to a project boundary.
- **Added** `GHOSTVOLUMES_AUTO_YES` to opt back into fully-automatic conversion (not recommended — see the FAQ).
- **Added** a startup guard: `ghostvolumes` now refuses to run at all if its own shim is already present in `LD_PRELOAD`, instead of silently misbehaving.
- **Changed** the compiled shim's filename to `libghostvolumes_shim.so` (from a generic `preload.so`), for unambiguous identification in `LD_PRELOAD`/`ps`/`/proc/*/maps` output.
