# GhostVolumes — Progress

Tracks implementation of `ai-work/tasks/main-plan.md`, broken into small compilable units. Each step: implement → test → commit source → update this file → commit this file. Never move to the next step while a test is red.

**Environment note (updated 2026-07-07):** this sandbox is rootless AlmaLinux 9 under Podman. Initial assumption was "no BTRFS available at all" (per user's initial guidance) — **this turned out to be only half true.** `/` and `/tmp` are `overlay` (container overlay fs), but `/root` itself is BTRFS-backed (`/dev/sda`, real persistent pod storage per its mountinfo path) despite `btrfs-progs` not being installed. Confirmed via a one-shot capability probe (raw `BTRFS_IOC_SUBVOL_CREATE` ioctl, dependency-free Rust compiled with bare `rustc`, mirroring how the real shim will be built): the ioctl **succeeds** in this rootless container, the created directory has inode 256 as expected, and `rmdir` on both the subvolume and its parent succeeded per the plan's kernel-4.18 note. Probe was run against a throwaway `/root/.ghostvolumes-scratch-test/` dir, fully cleaned up, nothing committed.
**Practical consequence:** tests needing real BTRFS behavior (subvolume creation, inode-256 checks, `scan`'s `.snapshots` detection, `discover`, `convert`, LD_PRELOAD injection) can run for real in this sandbox — but only if their scratch directories land on a BTRFS filesystem, NOT via `tempfile::tempdir()`, which defaults to `/tmp` (non-BTRFS overlay) and would silently make these tests exercise the wrong filesystem. Steps 9 onward use a scratch helper for BTRFS-dependent tests instead of `#[ignore]`-marking them.
**Portability fix (2026-07-07, prompted by user review):** that helper originally hardcoded `/root/.ghostvolumes-test-scratch`, which only worked because this specific sandbox happens to have `/root` as its BTRFS-backed path — not portable to any other machine/user. Fixed in both `src/test_support.rs` and `tests/shim_ld_preload.rs` (duplicated, since the latter is a separate integration-test binary with no lib target to import from): defaults to `<CARGO_MANIFEST_DIR>/target/ghostvolumes-test-scratch` — the project's own build directory, BTRFS-backed whenever the checkout itself is — overridable via `GHOSTVOLUMES_TEST_BTRFS_DIR` for a checkout that isn't. Verified the override actually redirects (pointed it at a known-non-BTRFS scratchpad path and confirmed the resulting ioctl failure — "Inappropriate ioctl for device" — proving the new location took effect, since a real BTRFS path would have succeeded there instead). Then went further per a second review comment: `shim_ld_preload.rs`'s `.env("HOME", ...)` used the real current user's `$HOME` as a placeholder — replaced with a deliberately fake, nonexistent path (`/nonexistent-fake-home-for-ghostvolumes-tests`), since these tests always override `XDG_DATA_HOME` explicitly (so `HOME`'s value never affects test logic) and a fake value makes any future accidental fallback to `$HOME`-derived paths fail loudly instead of silently touching the real developer's home.

**Loopback BTRFS test path (2026-07-07, discussed with user):** investigated whether tests could create their own throwaway BTRFS filesystem (`truncate` + `mkfs.btrfs` + loop-mount) instead of depending on the test machine already having one mounted somewhere. Confirmed empirically in this sandbox: `mkfs.btrfs` on a plain file needs no privilege at all, but every mount attempt — direct, `-o loop`, and even inside a fresh `unshare --user --map-root-user --mount` namespace — fails with "Operation not permitted," because this container has neither `CAP_SYS_ADMIN` nor `/dev/loop-control` exposed at all (confirmed via `/proc/self/status`'s capability bitmask and `ls /dev/loop*`). `chroot` doesn't help either concern raised (doesn't grant mount privilege; already unnecessary for faking `$HOME`, which the whole suite already does via plain env var overrides). Added `tests/btrfs_loopback.rs` as an opt-in (`#[ignore]`d) path for environments that *do* have real mount privilege (a CI runner, or a dev machine with root) — reuses the actual `shim/btrfs_core.rs` via a small probe binary (not a reimplementation), gracefully SKIPS (doesn't fail) if `mkfs.btrfs`/`unshare`/`mount` aren't available or permitted, and one always-run test (`skip_path_actually_triggers_in_this_environment`) guards the skip-detection logic itself against silently rotting. Validated as thoroughly as this sandbox allows: the skip-detection path is confirmed accurate here (correctly identifies the real "Operation not permitted" failure rather than masking it), and the probe binary's own logic was separately validated by running it directly against this sandbox's real BTRFS location (all 4 scenarios pass) — only the actual loopback-mount success path is unverified, since no environment with the needed privilege was available to test against.

Branch: `claude-ghostvolumes-impl`

---

## Step 1 — Project scaffold
**Status**: done
**Date**: 2026-07-07
### What was done
`cargo init`, added deps (`clap`+derive, `serde`+derive, `toml`, `anyhow`; dev-deps `tempfile`, `assert_cmd`, `predicates`). `src/main.rs` has a clap-derive CLI with subcommand stubs for `scan`, `reload`, `init`, `discover`, `convert`, `shell-init`, `ensure`, each returning `anyhow::bail!("... not implemented yet")`. Smoke tests in `tests/cli_scaffold.rs` verify `--help` lists every subcommand, no-args fails with usage, and an unimplemented subcommand fails cleanly with a readable message. `cargo test` green (3 passed).
### Deviations from plan
Edition 2024 (cargo's default for this rustc version) for the main crate — unrelated to the shim's own `--edition 2021` rustc invocation in a later step, no conflict.
### Issues found / fixed
None.

## Step 2 — Config data model + TOML parsing
**Status**: done
**Date**: 2026-07-07
### What was done
`src/config.rs`: serde structs `RootsFile`, `WatchedFile`, `ProjectsFile`/`ProjectEntry`, `RepoLocalFile`, matching the four fragment kinds in plan §2. Parse functions wrap `toml::from_str`. Unit tests cover the exact TOML examples from the plan (roots auto/local, watched defaults, projects local with two entries), missing-optional-field defaults, malformed TOML, and a `[[project]]` missing the required `path` field. 8/8 tests pass; prior 3 scaffold tests still pass.
### Deviations from plan
Modeled `.ghostvolumes.toml` as `{watch, proactive}` (no `path` field) since the plan doesn't give an explicit example — `path` is implicit (the directory the file is found in), consistent with how §6 describes registration.
### Issues found / fixed
None. (Dead-code warnings for the new parse functions are expected — nothing calls them yet; resolved once Step 3's merge logic uses them.)

## Step 3 — `*.d/` merge logic (§2)
**Status**: done
**Date**: 2026-07-07
### What was done
`src/merge.rs`: `load_all(config_dir)` loads and merges `roots.d/`, `watched.d/`, `projects.d/`. Roots/watched are `BTreeSet`-deduped unions across lexically-sorted `.toml` files (deterministic sorted output). Projects concatenate `[[project]]` entries keyed by `path` — a duplicate path across files is resolved by last-file-wins (lexical order). Missing `*.d/` dirs yield empty results, not errors. Non-`.toml` files in a `*.d/` dir are ignored. 7 new tests, all passing; 18/18 total.
### Deviations from plan
Plan doesn't specify what happens on a duplicate `[[project]] path` across multiple `projects.d/*.toml` files (only a single `local.toml` is shown in §2's example tree). Chose last-file-wins by lexical order, consistent with the `00-`/`10-` generated-then-hand-edited convention used elsewhere — documented in the module doc comment.
### Issues found / fixed
None.

## Step 4 — Path-matching logic (§4)
**Status**: done (amended 2026-07-07, see below)
**Date**: 2026-07-07
### What was done
`src/pathmatch.rs`: `resolve_watch_names` (global defaults ∪ project watch ∪ project proactive, or global defaults alone if no project matches an ancestor) and `resolve_proactive_names` (project's raw proactive names, or empty if no match). Project matching uses `Path::starts_with` for component-aware prefix matching so e.g. `.../big-frontend2` doesn't falsely match a `.../big-frontend` project. Nested project entries resolve via longest-prefix-wins. 7 new tests (no-match, union, prefix-boundary, nesting, exact-root, proactive empty/non-empty). 22/22 total passing.
### Deviations from plan
Plan's §4 pseudocode embeds the git-tracked filter inside `resolve_proactive_names` itself. Split that out: this module returns raw candidate names only (pure, FS-free, fully unit-testable); the git-tracked filter (Step 5) is composed at the `ensure` call site instead. Also: plan doesn't specify a tie-break for nested `[[project]]` entries — picked longest-prefix-wins (most specific match), documented in the module doc comment.
### Issues found / fixed
**Amendment (2026-07-07, prompted by user review):** both functions were missing the root check entirely — they'd return global defaults for *any* path, anywhere on the filesystem, once a `[[project]]` list existed. Added `roots: &[String]` as an explicit parameter to both, checked first and unconditionally (`is_under_any_root` helper), rather than documenting it as a caller precondition. 4 new tests added (root-restricted rejection even with a project match, multi-root matching, `is_under_any_root` boundary cases); all existing tests updated to pass `["/"]` (neutral "matches everywhere") and still pass unchanged. See plan §4's revised pseudocode.

## Step 5 — Git-tracked gate (§4)
**Status**: done
**Date**: 2026-07-07
### What was done
`src/git.rs`: `is_git_tracked(path)` walks `path.ancestors()` for the nearest `.git` (file or dir, so worktrees/submodules count too), then runs `git -C repo_root ls-files -- <relative-path>`; tracked iff the command succeeds with non-empty stdout. No repo found, or `git` unrunnable, both resolve to `false` rather than an error. Tests use real `git init`/`add`/`commit` in tempdirs (`git` is available in this sandbox): tracked file, untracked file, path outside any repo, nonexistent never-tracked path, nested-directory repo-root discovery, and missing-git-binary tolerance (via an injectable binary name rather than mutating the process's `PATH`, which would be unsafe across parallel `cargo test` threads). 5 new tests, 28/28 total.
### Deviations from plan
None functionally; internal-only refactor of `is_git_tracked` into a `_with_bin` variant purely to make the missing-binary path testable deterministically.
### Issues found / fixed
None.

## Step 6 — Flat compiled-cache format: writer + reader (§8.0)
**Status**: done (amended 2026-07-07, see below)
**Date**: 2026-07-07
### What was done
`src/cache.rs`: `compile(MergedConfig) -> String` renders `compiled.tsv` text; `parse(text) -> Vec<(prefix, name)>` reads it back (same `str::split('\t')` the dependency-free shim will use in Step 12); `names_for(rows, path)` unions the names of every row whose prefix is an ancestor of `path`. A round-trip test compiles a sample config, parses it back, and asserts `names_for` produces byte-for-byte the same result as Step 4's `pathmatch::resolve_watch_names` across several paths (no-match, project match, nested, sibling-prefix false positive) — confirms the delta representation is equivalent to full per-project expansion. 4 new tests, 32/32 total.
### Deviations from plan
Resolved an ambiguity: plan prose in §8.0 calls the cache "the fully expanded result of `global_defaults ∪ project.watch ∪ project.proactive` per project," but the literal example directly below it only lists each project's own `watch`/`proactive` names, not the global defaults again. Implemented per the literal example (delta rows, not full expansion) since a reader has to do ancestor-prefix matching across multiple rows regardless (a project's own rows can't be a flat exact-match lookup on their own, since arbitrary nested subdirectories under the project also need to match), so re-listing global names under every project would only add duplication with no simplification benefit. `names_for`'s ancestor-union matching reproduces the exact same result as full expansion, confirmed by the round-trip test above.
### Issues found / fixed
Originally flagged here as "not blocking": neither `compiled.tsv` nor §4's resolve functions referenced `roots` at all. **User review determined this was a real bug, not an acceptable gap** — reactive matching must be gated on configured roots, as the cheapest possible first-line rejection (LD_PRELOAD intercepts every `mkdir`/`mkdirat` system-wide, so most calls need to be rejected as cheaply as possible). Fixed:
- `cache::compile()` now keys global-default rows by each entry in `roots`, not a hardcoded `/`. Plan §8.0 updated to match (example now shows `roots = ["/home/user1", "/data/workspaces"]` producing root-keyed rows, degenerating to the old `/`-prefixed form only when `roots == ["/"]`).
- New `src/btrfs.rs`: `is_btrfs(path)` via `statfs`'s filesystem magic number, for validating every configured root is still BTRFS-backed at `reload` time (Step 7) — plan §8.0 updated to require `reload` fail loudly on a stale (no-longer-BTRFS) root rather than silently compiling a cache that references a filesystem that can't take a `BTRFS_IOC_SUBVOL_CREATE`. Added `libc` as a direct dependency of the main CLI for the raw `statfs` call (the dependency-free constraint from §8.1 applies only to the shim, not the main crate). 2 new tests: confirms this sandbox's tempdir is correctly detected as non-BTRFS (real syscall exercised, sandbox has no BTRFS to test the positive case — flagged for a real BTRFS box), and that a nonexistent path errors rather than panicking.
- Plan §4's pseudocode updated to show the root check as an explicit, first, unconditional step in both `resolve_watch_names`/`resolve_proactive_names` (see Step 4's amendment above) rather than prose describing it as happening "elsewhere."
5 new tests across `cache.rs`/`pathmatch.rs`/`btrfs.rs` for this fix; 38/38 total passing.

## Step 7 — `reload` subcommand (§8.0)
**Status**: done
**Date**: 2026-07-07
### What was done
`src/xdg.rs`: resolves `~/.config/ghostvolumes` / `~/.local/share/ghostvolumes` honoring `XDG_CONFIG_HOME`/`XDG_DATA_HOME`; pure logic takes `home`/override as arguments (no env mutation in tests), thin wrappers read the real environment. `src/reload.rs`: `reload(config_dir, cache_path)` runs `merge::load_all` → validates every root is BTRFS-backed (via `btrfs::is_btrfs`, injectable as `reload_with_validator` for testability) → `cache::compile` → atomic write (temp file in the same dir + `rename`). A validation failure bails with a message naming the bad root and suggesting `scan --save`, and leaves any pre-existing cache file untouched. Wired into `main.rs`'s `Command::Reload` arm. CLI-level tests override `$HOME` to a tempdir so they never touch the real user's XDG directories. 10 new tests (5 `reload.rs` unit, 4 `xdg.rs`, 2 CLI/`assert_cmd`), 53/53 total.
### Deviations from plan
None beyond the roots-validation addition already captured in Step 6's amendment.
### Issues found / fixed
The scaffold's `unimplemented_subcommand_fails_cleanly` test used `reload` as its "still a stub" example; now that `reload` is real (and touches `$HOME`-derived paths when run for real), switched that test to use `init` instead — caught before it could silently start writing to the real `$HOME/.local/share/ghostvolumes/` during `cargo test` runs.

## Step 8 — mountinfo parsing (§3, pure function)
**Status**: done
**Date**: 2026-07-07
### What was done
`src/mountinfo.rs`: `btrfs_mountpoints(text) -> Vec<String>` parses `/proc/self/mountinfo` (format per `man 5 proc`), splitting each line on `" - "` to separate the variable-length optional-fields section from the trailing `fstype source options`, then filters to `fstype == "btrfs"`. Handles octal-escaped mountpoints (`\040` for space, etc). Malformed lines are skipped, not errored — best-effort discovery. 6 new tests (extraction, no-matches, empty input, malformed lines, zero/multiple optional fields, octal unescaping). 54 unit + 3 + 2 CLI = 59 total passing.
### Deviations from plan
None.
### Issues found / fixed
None. Side note: sanity-checked against this sandbox's real `/proc/self/mountinfo` and found `/root` itself is BTRFS-backed (`/dev/sda`) despite no `btrfs-progs` CLI being installed — following up separately on whether the subvolume-creation ioctl is actually usable here before Step 9, since that could change the testing story for the remaining BTRFS-dependent steps.

## (Infrastructure, ahead of Step 9) Real BTRFS subvolume primitives
**Status**: done
**Date**: 2026-07-07
### What was done
Prompted by the capability-probe finding above: `src/btrfs.rs` gained `is_subvolume(path)` (inode-256 check) and `create_subvolume(parent, name)` (hand-declared `BTRFS_IOC_SUBVOL_CREATE` ioctl — struct layout and `_IOW` request-number computation matching `<linux/btrfs.h>`, same approach the dependency-free shim will use later). `src/test_support.rs` added `btrfs_scratch_dir()`, a `tempfile::tempdir_in("/root/.ghostvolumes-test-scratch")` wrapper — `/root` is real BTRFS here, `/tmp` (the default tempdir location) is container overlayfs. `btrfs.rs`'s tests were upgraded from "documents the positive case needs a real BTRFS box" to actually exercising it: real detection, real subvolume creation/detection, duplicate-name and missing-parent failures. 6 new tests, 64/64 total passing (59 unit + 3 + 2 CLI). Confirmed `TempDir`'s auto-cleanup correctly removes created subvolumes (not just plain directories) on drop.
### Deviations from plan
None — this is infrastructure the plan assumed would eventually be needed (§5's `BTRFS_IOC_SUBVOL_CREATE`, §3/§7's inode-256 checks), pulled forward once real BTRFS testing became available instead of being written for the first time inside Step 9-14 with only `#[ignore]`d tests.
### Issues found / fixed
None.

## Step 9 — `scan` / `scan --save` (§3)
**Status**: done
**Date**: 2026-07-07
### What was done
`src/scan.rs`: `detect_roots()` reads `/proc/self/mountinfo`, finds BTRFS mountpoints (Step 8's `mountinfo::btrfs_mountpoints`), and keeps only those with a `.snapshots` child that's itself a subvolume (`btrfs::is_subvolume`, inode 256) — Snapper's structural fingerprint, correct for both openSUSE (separately-mounted `.snapshots`) and Arch (nested) layouts since it's a plain `stat()` either way. `save_roots()` serializes via `RootsFile`/`toml::to_string` (not hand-formatted strings — guarantees round-trip correctness with `config::parse_roots`) and writes only `roots.d/00-auto.toml` via the new shared `atomic_write::write_atomically` (extracted from `reload.rs`, now used by both). Wired into `main.rs`: `scan` prints detected roots (dry run); `scan --save` writes `roots.d/00-auto.toml` then calls `reload::reload()` — the "automatic at the end of `scan --save`" regeneration trigger from §8.0. 8 new real-BTRFS tests (snapshot-subvolume detected, plain-dir-named-`.snapshots` rejected, non-BTRFS-mountpoint rejected even with a `.snapshots` dir, sort/dedup, `10-local.toml` left untouched, full save→reread-via-`merge::load_all` round trip) + 4 for the extracted `atomic_write` module. 70 unit + 3 + 2 CLI = 75 total. Manually ran `cargo run -- scan` against this sandbox's real `/root` (BTRFS, not Snapper-managed) — correctly returned empty with exit 0.
### Deviations from plan
Implemented only §3 point 1 (unprivileged detection). §3 point 2 (privileged pass under `sudo`: `snapper list-configs`, `/etc/timeshift/timeshift.json`, `/etc/btrbk/btrbk.conf`) is out of scope for this step — no sudo/privileged context to test against in this sandbox, and it's a separable, additive enhancement to `detect_roots()` rather than something other steps depend on.
### Issues found / fixed
None.

## Step 10 — `discover` (§7)
**Status**: done
**Date**: 2026-07-07
### What was done
`src/discover.rs`: `walk()` stat-walks from a start path matching watched-name subvolumes (skips `.git`, never descends into a watched-name match — subvolume or not). `group_and_gate()` groups by parent and partitions through the git-tracked gate (annotated-not-omitted per §7). `format_toml()` renders `[[project]]` blocks (verified to round-trip through `config::parse_projects`) plus skip-comments for git-tracked names. Wired into `main.rs`: dry run prints, `--save` appends to `projects.d/local.toml` (never overwrites) then calls `reload()`. 12 new tests, all against real BTRFS subvolumes via the scratch helper. 81 unit + 3 + 2 CLI = 86 total. Manually verified end-to-end with a subvolume created outside the test harness (real `ioctl`, not `cargo test`) — `discover` found it and produced valid TOML; all manual-check state cleaned up afterward.
### Deviations from plan
### Issues found / fixed

## Step 11 — `convert` (§7)
**Status**: done
**Date**: 2026-07-07
### What was done
`src/convert.rs`: refuses git-tracked paths outright (no override, §1), plus two extra guards beyond the plan's literal text — already-a-subvolume and not-a-directory (documented as deliberate additions). Creates a new subvolume at a hidden temp sibling (`.<name>.ghostvolumes-convert-tmp`), shells out to `cp -a --reflink=always -- <path>/. <tmp>` (not reimplemented in Rust — matches the plan's explicit choice of that exact command), then does an atomic swap: rename old dir to a hidden backup name, rename new subvolume into place, remove the backup. `path` is never missing or half-written between the two renames. 9 new tests against real BTRFS: content preservation (nested dirs + top-level files), no leftover tmp/backup dirs, empty-dir edge case, git-tracked/nonexistent/non-directory/already-subvolume refusals, permission preservation via `cp -a`, and confirms the post-convert inode is genuinely new (256), not the old directory's inode. Ran `cargo fmt` + `cargo clippy --all-targets` (per the new standing practice from this point on) — clean except the same pre-existing Step 14 dead code. 95/95 total (90 unit + 3 + 2 CLI).
### Deviations from plan
### Issues found / fixed

## Step 12 — LD_PRELOAD shim source + `build.rs` compile + `init` extraction (§8.1)
**Status**: done
**Date**: 2026-07-07
**Real BTRFS available** via the `/root`-rooted scratch helper: end-to-end `mkdir`/`mkdirat` interception → `BTRFS_IOC_SUBVOL_CREATE` can be tested for real (confirmed working via the capability probe in the environment note above). The shim's config-parsing logic (flat TSV reader) is dependency-free plain Rust and gets normal unit tests regardless.

### 12a — extract shared dependency-free logic into shim/
Discussed at length with the user first: "dependency-free" (plan §8.1) means no crates.io crates specifically, not no `std` — bare `rustc` links `std` automatically (bundled with the toolchain), so the constraint only bites `libc`/`serde`/`anyhow`/etc, which need a Cargo-resolved `.rlib` bare `rustc` has no way to find. Also resolved: `cargo binstall` would reintroduce the libc-matching problem the build-on-target-machine design avoids, since it skips `build.rs` and downloads a CI-built binary — plan §8.1 now says GhostVolumes shouldn't publish binstall-compatible release artifacts.

Moved `compiled.tsv` `parse`/`names_for` (from `cache.rs`), the git-tracked gate (all of `git.rs`, already 100% dependency-free as written), and BTRFS subvolume detection/creation (`is_subvolume`/`create_subvolume` from `btrfs.rs`, rewritten from `libc`+`anyhow` to hand-declared `unsafe extern "C"` for `open`/`close`/`ioctl` + `std::io::Result`) into `shim/{cache_core,git_core,btrfs_core}.rs`. The main crate pulls each in verbatim via `include!("../shim/....rs")` rather than maintaining a parallel implementation. `is_btrfs` (statfs-based filesystem-type check) stays CLI-only in `src/btrfs.rs`, still using the `libc` crate freely, since root validation happens at `reload` time (§8.0), not on the shim's hot path — the shim never needs it.

Verified each `shim/*.rs` file compiles standalone via `rustc --edition 2021 --crate-type lib` (simulating how `shim/preload.rs` will consume them via `mod`), in addition to the normal `cargo build`/`cargo test`. `#[cfg(test)]` blocks inside the shared files are automatically stripped by the shim's plain `rustc` invocation (no `--cfg test`), so test code didn't need separating out.

No behavior change: 96 unit + 3 + 2 CLI = 101 tests, same count, all still green. `cargo fmt` + `cargo clippy --all-targets` clean (same expected dead-code warnings pending Step 14's `ensure` wiring). Also shared XDG dir resolution the same way (`shim/xdg_core.rs`) since the shim must resolve `compiled.tsv`'s location exactly like `reload`/`init` do, or a custom `XDG_DATA_HOME` would silently break it.

### 12b — shim/preload.rs (the actual FFI glue)
Hand-declares only `dlsym` via `unsafe extern "C"` — everything else (reading `compiled.tsv`, `stat`, `current_dir()`, `/proc/self/fd` via `read_link`) uses plain `std`. Exports `mkdir`/`mkdirat` via `#[no_mangle]`, resolving the real functions once via `dlsym(RTLD_NEXT, ...)` cached in a `OnceLock`. One-time cache load via a hand-written `#[link_section = ".init_array"]` constructor. Decision order matches §5 exactly: `cache_core::names_for` (root gating + name matching in one pass) → `btrfs_core::is_subvolume` (cheap stat) → `git_core::is_git_tracked` (most expensive, checked last). Every failure path degrades to passthrough, never panics.

Verified end-to-end by hand first: compiled the shim via the exact bare-`rustc` invocation `build.rs` will use, then really `LD_PRELOAD`ed it against real `mkdir`/`mkdirat` calls on real BTRFS subvolumes — confirmed matching names become subvolumes (inode 256), non-matching/outside-root names stay plain, relative paths resolve via cwd, `mkdirat`'s dirfd resolves via `/proc/self/fd`, the git-tracked gate blocks a deleted-but-still-git-tracked directory, and calling `mkdir` again on an already-created subvolume correctly falls through to the real `mkdir` and reports `EEXIST` normally (not a special "success" case — verified separately via a standalone probe that `BTRFS_IOC_SUBVOL_CREATE`'s `EEXIST` errno (17) correctly maps to `std::io::ErrorKind::AlreadyExists`, which is what `try_create_subvolume`'s own EEXIST-tolerance branch relies on). All manual-exploration artifacts (scratch dirs, compiled `.so`, probe binaries) cleaned up before committing.

Encoded as `tests/shim_ld_preload.rs`: 8 automated end-to-end tests reproducing every scenario validated by hand above, so this stays a regression test rather than one-off manual confidence. 109/109 total (96 unit + 3 + 2 + 8 shim integration). `cargo fmt` + `clippy` clean.

### 12c — build.rs compiles the shim; init extracts it
`build.rs` shells out to bare `rustc` (never `cargo build`) to compile `shim/preload.rs` into `$OUT_DIR/preload.so`, targeting `$TARGET` (cargo's own target triple) with the musl `-crt-static` override for `cdylib` compatibility, `rerun-if-changed` on all `shim/*.rs` files. `src/init.rs` embeds the result via `include_bytes!` and does zero compilation at runtime — `ghostvolumes init` just extracts those bytes to `~/.local/share/ghostvolumes/preload.so` and writes default config skeletons (the three `*.d/` directories, `watched.d/00-defaults.toml` only if absent — never overwrites a customization). Updated the scaffold's "unimplemented subcommand" test to use `shell-init` instead of `init`, since `init` is no longer a stub.

Verified the *complete* pipeline end-to-end manually, in the exact sequence a real user runs after `cargo install`: `cargo build` (build.rs compiles the real shim) → `ghostvolumes init` (extracts it) → `ghostvolumes reload` (compiles a real `compiled.tsv`) → `LD_PRELOAD`ing the init-extracted `.so` against a real `mkdir` call → a real BTRFS subvolume got created. All manual-test state cleaned up before committing.

6 new `init.rs` tests, 115/115 total (102 unit + 3 + 2 + 8 shim integration). `cargo fmt` + `clippy` clean.

**Step 12 complete.** All three sub-steps (12a shared dependency-free logic, 12b shim FFI glue, 12c build.rs/init wiring) done and verified end-to-end against real BTRFS in this sandbox.
### Deviations from plan
### Issues found / fixed

## Step 13 — `shell-init` (§8.2)
**Status**: done
**Date**: 2026-07-07
### What was done
`src/shellinit.rs`: prints (never writes) a shell snippet, matching `starship`/`zoxide`/`direnv`. `LD_PRELOAD` is exported by appending to any existing value (`${LD_PRELOAD:+$LD_PRELOAD:}...`) rather than clobbering it. bash gets a `cd()` function wrapping `builtin cd`; zsh gets an `add-zsh-hook chpwd` registration (the idiomatic zsh mechanism — cleaner than shadowing `cd` there). Both call `ghostvolumes ensure "$PWD"` after every directory change. Wired into `main.rs`. Updated the scaffold's "unimplemented subcommand" test again (`ensure` is now the only remaining stub). 8 new tests, 122/122 total (109 unit + 3 + 2 + 8 shim integration). `cargo fmt` + `clippy` clean.
### Deviations from plan
None.
### Issues found / fixed
Flagged, not fixed: the bash snippet is validated for real via `bash -n` (this sandbox has bash). The zsh snippet is only checked structurally (string content), since zsh isn't installed here — worth running `zsh -n` on it on a machine that has zsh, before relying on it.

## Step 14 — `ensure` / cd-hook (§6)
**Status**: done
**Date**: 2026-07-07
**Pre-fix landed:** discovered while starting this step that `compiled.tsv` (Steps 6/9) couldn't actually support it — it stored an undifferentiated `watch ∪ proactive` union, but `ensure` needs specifically the `proactive` subset (§4: never proactively create a `watch`-only name), *and* the project's root path (since names get created there, not necessarily at `$PWD`). Fixed by adding an optional third `proactive` marker column to `compiled.tsv`, and a single `cache::proactive_project_for()` (superseding an earlier, incomplete `proactive_names_for` draft) returning `(project_root, proactive_names)`.
### What was done
`src/ensure.rs`: no project match → pure no-op immediately. Per matched proactive name: missing → real subvolume created; already a subvolume → no-op; plain directory → left alone, warned once per session via a marker at `${XDG_RUNTIME_DIR:-/tmp}/ghostvolumes/warned/<pid>-<hash>`, where the PID is the invoking shell's own — recovered via `getppid()` since `ensure`'s immediate parent process *is* that shell, so `shell-init`'s snippet needed no changes. `src/registration.rs`: `.ghostvolumes.toml` registration — one cheap `stat()` at `$PWD` (deliberately unconditional on an existing project match, to avoid a chicken-and-egg problem for a repo's first-ever registration), tracked via a small `registered.tsv` registry keyed by mtime, folded into an auto-managed `projects.d/00-repo-local.toml` (replaces a stale entry for the same path rather than duplicating; accumulates across multiple registered repos), then `reload()`s so the same `ensure` call sees the freshly-registered project.

Marked `pathmatch.rs`'s functions and `cache_core::names_for` `#[allow(dead_code)]` with corrected doc comments — both are now permanently one-sided (a tested reference spec never called by production code; alive only in the shim's separate compilation), not "pending a future step," so leaving them warning would be noise forever. This closes out `config.rs`'s `RepoLocalFile`/`parse_repo_local`, unused since Step 2 — **`cargo clippy` is now fully clean, zero warnings**, for the first time since early in this session.

Verified end-to-end manually via the real CLI: proactive creation, idempotent re-run, warn-once dedup by parent PID, and full `.ghostvolumes.toml` registration (registry file, generated `00-repo-local.toml`, immediate effect on the same `ensure` invocation). 17 new tests across `ensure.rs`/`registration.rs`. 141/141 total (128 unit + 3 + 2 + 8 shim integration).
### Deviations from plan
Simplified the `.ghostvolumes.toml` registration trigger: plan §6 says "on a project-root match," but registering unconditionally on a cheap `stat()` (rather than gating behind an existing match) is what actually makes a repo's first-ever registration possible at all — see pre-fix note above and the dedicated commit message.
### Issues found / fixed
None beyond the pre-fix.

**Steps 1-14 complete.** Remaining: Step 15 (platform gating + packaging polish) and Step 16 (deferred, out of scope for this phase).

## Step 15 — Platform gating + packaging polish (§8.3, §8.4)
**Status**: done
**Date**: 2026-07-07
### What was done
Every module in `main.rs` gated behind `#[cfg(target_os = "linux")]`; non-Linux gets a clean "GhostVolumes only supports Linux with BTRFS" message + `exit(1)` instead of a confusing compile error. `build.rs` skips the shim `rustc` invocation entirely off-Linux. Verified for real by cross-compiling to `wasm32-unknown-unknown` (a genuinely non-Linux target) — builds clean, pulling in only `clap`/`anyhow`/`libc`, none of the gated modules. `Cargo.toml` gained crates.io metadata (description, `license = "MIT OR Apache-2.0"`, readme, keywords, categories) and a `README.md` (install/setup, command reference, config layout, upgrade instructions, known limitations). Validated with `cargo publish --dry-run --allow-dirty`: packages cleanly and the verification build succeeds from the packaged tarball (confirms `build.rs` finds `shim/` correctly post-packaging). 141/141 tests still passing, `cargo fmt` + `clippy` fully clean (zero warnings).
### Deviations from plan
Left the `license` choice (MIT OR Apache-2.0, the standard Rust ecosystem dual-license default) and omitted `repository` (no real GitHub URL exists yet — left unset rather than fabricated) as decisions for the user to confirm/override; not specified in the plan itself.
### Issues found / fixed
None.

**All 15 active steps of the plan are now complete and verified**, most against real BTRFS in this sandbox (the `/root` discovery from Steps 8-9 made this possible instead of `#[ignore]`d stubs). 141 tests total. `cargo fmt` + `cargo clippy --all-targets` fully clean.

## Step 17 — Shim debug logging (§8.5)
**Status**: done
**Date**: 2026-07-07
Added after the initial 15-step implementation, per explicit user request.
### What was done
`GHOSTVOLUMES_DEBUG` (any value other than empty/`"0"` enables it) and `GHOSTVOLUMES_LOG_FILE` (default `~/.local/share/ghostvolumes/shim.log`) — pure environment variables, read live via `std::env::var` at process start. Normal mode logs only critical events (subvolume created, unexpected creation errors). Debug mode logs every `mkdir`/`mkdirat` decision and why: `should_intercept` restructured into a `Decision` enum (`Accept`/`AlreadySubvolume`/`GitTracked`/`NoCacheMatch`) so the reasoning is explicit. Never prints to stdout/stderr under any circumstances — verified by a test that captures output directly, not just infers it from success. 5 new end-to-end tests in `tests/shim_ld_preload.rs`. 146/146 total (128 unit + 3 + 2 + 13 shim integration). `cargo fmt` + `clippy` fully clean.
### Deviations from plan
**First implementation attempt used `settings.toml` → compiled `shim.conf`** (mirroring `compiled.tsv`'s "CLI parses TOML, shim reads a flat compiled file" pattern) — written, tested, then **fully reverted** after the user pushed back mid-implementation questioning whether that complexity was warranted for two simple settings. Discussion surfaced a genuine correctness trap in the *alternative* of reusing `compiled.tsv` itself (an empty-string sentinel prefix for settings rows would make `Path::starts_with("")` — true for every path — corrupt `names_for`'s matching for every intercepted call), which settled the question in favor of the simplest option: pure env vars, no file format, no parser, no compiled artifact, no staleness class. Plan §8.5 rewritten to document both the final design and why the TOML-routed one was rejected, rather than silently deleting the reasoning.
### Issues found / fixed
None beyond the above design pivot.

## Step 16 — (Deferred) seccomp-notify supervisor
**Status**: not started — deferred per plan §1/§9, only if real-world need appears
**Date**:
### What was done
### Deviations from plan
### Issues found / fixed
