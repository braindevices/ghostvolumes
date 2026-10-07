# Audit fixes — progress
Plan: [audit-fixes.plan.md](audit-fixes.plan.md)

## Step 1 — Parser cannot panic; one pattern-validity predicate (F3, F6, N5)
**Status**: done
**Date**: 2026-10-07
### What was done
- `parse_lines`: strips a BOM, uses `strip_prefix('+'/'-')` instead of `split_at(1)` (can no longer panic).
- New `decision_core::valid_pattern`: no `.`/`..` components, no control chars. Applied in `parse_lines`, `parse_anchored_exact_patterns`, `anchored_pattern` (returns None), and `decide --add/--deny` (errors).
- `convert.rs` undecided / already-subvolume paths skip a candidate whose name can't be recorded instead of falling back to its display path.
- Tests: 5 decision_core unit tests, `decide_rejects_dot_dot_and_control_char_patterns_without_writing`, shim `a_bom_or_non_ascii_decision_file_never_aborts_the_host_process`.
### Deviations from plan
- Empty components (`a//b`) are still accepted (already harmless: matching filters them).
### Issues found / fixed
- None.

## Step 2 — Atomic shim install, plus a stale-shim warning (F4)
**Status**: done
**Date**: 2026-10-07
### What was done
- `write_atomically` takes `impl AsRef<[u8]>`; `init` installs the shim through it (temp + rename).
- `init::shim_is_current` (size, then bytes vs embedded `PRELOAD_SO`) + `warn_if_shim_stale` (stderr) called by `intercept` and `shell-init`.
- README: init doesn't compile; Upgrading explains cargo install vs init and that running sessions keep the old shim.
- Tests: `rerunning_replaces_the_shim_by_rename_not_in_place` (inode changes, old fd unaffected), `shim_is_current_tracks_the_installed_bytes`.
- Manual repro (QA-4): python mkdir loop under LD_PRELOAD while running `init` 200x — survived (was SIGBUS 2/3 runs before).
### Deviations from plan
- None.
### Issues found / fixed
- None.

## Step 3 — Safe decision/ignore-file I/O (F2)
**Status**: done
**Date**: 2026-10-07
### What was done
- `decision_core::open_regular_file(path, append)`: lstat must be a regular file ≤ 1 MiB, open, re-check dev/ino on the fd; a missing file (append) is created with `create_new` (O_EXCL, never follows symlinks). `read_regular_file` wraps it.
- Switched: shim `read_decision_file` + `append_pending_marker`; convert `read_decision_file`, `read_ignore_file`, `decision_file_anchored_candidates`, `append_pending_marker`, `record_decision` (via new `existing_decision_text`: absent → empty, unsafe → error); discover, completions, intercept snapshot.
- Tests: `regular_file_io_refuses_symlinks_and_special_files` (symlink, /dev/zero, /dev/null, FIFO, dir, >1 MiB), `regular_file_io_reads_appends_and_creates`, convert `a_symlinked_decision_file_is_never_read_through_or_written_through` (TTY + non-TTY), shim `a_symlinked_decision_file_is_never_appended_through`.
### Deviations from plan
- No owner (euid) check: the threat is repo content, which the user owns after cloning anyway.
- Added a fail-fast `existing_decision_text` check before `materialize` in the interactive path, so an unsafe decision file aborts *before* converting rather than after.
### Issues found / fixed
- Found while testing: with a TTY, the original order converted first and only then failed to record the `+` — fixed by the fail-fast check above.

## Step 4 — Containment guard for `convert` mutations; `..` handling (F1, F13, real-dir part of F7)
**Status**: done
**Date**: 2026-10-07
### What was done
- `convert::check_contained(target, boundary)`, called first in `materialize` (the single choke point for `create_empty` and `copy_and_swap`): strictly under the boundary by `Normal` components, every existing component a real directory (symlink_metadata), target a real directory if it exists.
- `main::absolutize`: canonicalizes the deepest existing ancestor (kernel resolution of symlinks and `..`), appends the missing rest, errors on `..` in the rest.
- `projects register` requires an existing directory.
- Tests: `decision_patterns_never_convert_anything_outside_the_project` (`+ /../outside`, `+ /link/sub` via symlink, `+ /afile`, `--create` through a symlink — outside data untouched), `check_contained_accepts_only_plain_paths_strictly_inside`, `symlinks_and_dot_dot_resolve_like_the_kernel_not_lexically`, `dot_dot_after_a_missing_directory_is_an_error`.
### Deviations from plan
- Escape attempts make `convert` return an error (aborting the run) rather than skipping that candidate — clearer for a hostile repo, and the guard sits below the per-candidate loop.
- No separate CLI test for `register` on a missing path (3-line guard); covered by reasoning, not a test.
### Issues found / fixed
- Test fix: `--create` of an undecided path in non-TTY mode only leaves a marker, so the escape test needed a matching `+` line to actually reach `materialize`.

## Step 5 — Shim resolves targets by opened parent fd (F10, `..`, N3/F13 shim side, decide→create race)
**Status**: done
**Date**: 2026-10-07
### What was done
- Shim `resolve_target` replaces `resolve_path`/`dirfd_path`/`cwd`: last component checked first against a `WATCHED_NAMES` set (no syscalls for unwatched names, no `getcwd` at all); parent opened (relative to cwd, or `/proc/self/fd/<dirfd>/…` for mkdirat) behind an is_dir guard; real parent read back from `/proc/self/fd/<fd>`; ' (deleted)', no /proc, bad dirfd → pass through.
- `btrfs_core::create_subvolume_in(&File, name)`; the shim creates on the same parent fd it decided on. `create_subvolume(path, name)` kept for the CLI as a wrapper.
- `reload` compiles roots as canonical paths and calls new `projects::canonicalize_entries` (lock, canonicalize existing entries, dedupe, keep missing ones).
- Tests: shim `a_symlinked_parent_is_decided_and_created_at_its_real_path`, `dot_dot_in_the_target_resolves_like_the_kernel`, `dot_dot_through_a_symlink_follows_the_kernel_out_of_the_project`, `mkdirat_with_an_invalid_dirfd_fails_with_ebadf_instead_of_using_the_cwd`, `a_deleted_cwd_passes_through_to_the_real_mkdir`; debug test asserts an unwatched name never reaches ENTER; reload `roots_and_project_roots_are_compiled_as_physical_paths`. mkdirat probe now exits with errno and accepts BADFD.
- Checked against the previous shim: 4 of these tests fail there (symlinked parent, symlink+.., bad dirfd, unwatched ENTER).
### Deviations from plan
- `create_subvolume` kept as a path-taking wrapper rather than changing every CLI caller to open the parent (smaller diff, same guard).
- Unwatched-name check doesn't use `strace`; the debug-log ENTER assertion covers it.
### Issues found / fixed
- `a/../node_modules` already happened to work in the old shim (it read the decision file at `a/..`, which the kernel resolves correctly); kept the test as a regression guard.

## Step 6 — P0 doc corrections (F5 claim, F22)
**Status**: done
**Date**: 2026-10-07
### What was done
- design.md: the per-boundary lock claim corrected — it covers the shim's interception only, not ordinary writes; documents keeping the reflinked backup and stopping writers before converting.
- convert.md: `<path>` and anything outside it are never converted (`.`/`..` patterns ignored, symlinked/non-dir targets refused).
- intercept.md: example now actually triggers the shim (`intercept -- mkdir build`) and shows the real anchored marker (`? /build`).
- README's 'init compiles' / 'safe to re-run' fixed in step 2; 'shim never writes stderr' is true again after step 1 (no panic path).
### Deviations from plan
- design.md describes step 7's kept backup ahead of its implementation: **cut the release after step 7, not step 6.**
- No TOC change (no headings touched).
### Issues found / fixed
- None.

## Step 7 — `copy_and_swap` hardening (F5 code, F7, F15)
**Status**: done
**Date**: 2026-10-07
### What was done
- `copy_and_swap(path, delete_backup)`: cp source is `path.join(".")` (OsStr, no lossy display); a leftover tmp **or** backup dir stops it before any change; cp failure removes the tmp subvolume; a failed second rename renames the backup back; the backup is **kept by default** with an `rm -rf` hint, deleted only on request (cleanup failure is a warning, not an Err, so the `+` is still recorded).
- Config key `delete-convert-backup` (config.rs/merge.rs, last file wins) and `convert --delete-backup`; carried as `Mode::delete_backup` into `materialize`.
- The candidate walk skips `*.ghostvolumes-convert-{tmp,old}`.
- Tests: `the_backup_is_kept_by_default_and_deleted_when_configured` (replaces `no_leftover_backup_or_tmp_directories_after_success`), `leftover_convert_tmp_and_backup_dirs_are_never_candidates`, `an_existing_backup_stops_convert_before_anything_changes`.
- Manual repro (QA-3): 20k files + concurrent writer during `convert`: 57,070 successful writes, all present in new subvolume ∪ kept backup (was 24,299 lost).
### Deviations from plan
- No automated test for the cp-failure cleanup or second-rename rollback: there's no way to make `cp`/`rename` fail on demand without a global PATH override or fault injection; both are one-line best-effort branches.
- `cp` still resolved via PATH (won't-fix in the audit).
### Issues found / fixed
- Writes landing between the two renames fail with ENOENT in the writer (visible, not silent) — inherent to path-based swapping; documented by the kept backup.

## Step 8 — Shim mkdir semantics (F8, F9)
**Status**: done
**Date**: 2026-10-07
### What was done
- `CreateResult::Exists`: EEXIST from the ioctl now falls through to the real syscall (correct errno, no false 'created subvolume' log).
- After a successful create, `set_permissions(cur & mode & 0o7777)` — the ioctl's `0777 & ~umask` intersected with the requested mode = `mode & ~umask`. `mode` is threaded through `handle_intercept`.
- Tests: `mkdir_on_an_existing_file_or_plain_dir_reports_eexist_not_success` (python `os.mkdir` → FileExistsError for both; file content intact), `a_created_subvolume_honours_the_requested_mode` (0700 under umask 022).
### Deviations from plan
- Setgid-parent group inheritance still not replicated (QA noted it; known limitation, not in scope).
### Issues found / fixed
- None.

## Step 9 — No pending markers outside registered projects (F11)
**Status**: done
**Date**: 2026-10-07
### What was done
- Shim Undecided arm appends a `?` marker only when the boundary is a registered project root; otherwise logs only.
- intercept.md notes it.
- Tests: new `an_unregistered_project_gets_no_marker_at_the_volume_root`; the two existing marker tests now register their project first.
### Deviations from plan
- The CLI side needed no change: `convert`/`decide` always register the project before writing markers.
### Issues found / fixed
- None.

## Step 10 — Log opened per line (F12)
**Status**: done
**Date**: 2026-10-07
### What was done
- `LogContext` holds the log *path*, not an open fd; `log_line` opens (O_APPEND, O_CLOEXEC via std), writes one line, closes. Mutex removed (fixes the fork/signal-safety concern on the logging path).
- Test: `a_host_reusing_low_fds_never_receives_log_lines` (python `closerange(3,1024)` + reopen low fd + undecided mkdir → victim unchanged, line in shim.log).
### Deviations from plan
- None.
### Issues found / fixed
- None.

## Step 11 — `intercept` correctness (F14)
**Status**: done
**Date**: 2026-10-07
### What was done
- `intercept` appends the shim to an existing `LD_PRELOAD` (no duplicate if already present) via new `preload_value`; signal deaths return 128+N.
- Tests: the existing run test now asserts the child actually sees the shim in `LD_PRELOAD`; `a_signal_death_maps_to_128_plus_the_signal`; `preload_value_appends_once_and_keeps_existing_entries`.
### Deviations from plan
- `.so`-missing check not added here: step 2's stale-shim warning already fires ("missing or out of date") before `intercept` runs.
### Issues found / fixed
- ld.so 'cannot be preloaded' noise in these unit tests remains (they point at a non-existent .so on purpose).

## Step 12 — mountinfo bytes (F19)
**Status**: done
**Date**: 2026-10-07
### What was done
- `mountinfo::unescape` collects bytes and decodes with `from_utf8_lossy` (no Latin-1 re-encoding).
- Test: `unescape_keeps_utf8_and_octal_escaped_utf8_intact` (raw UTF-8, octal-escaped UTF-8, `\\040`).
### Deviations from plan
- None.
### Issues found / fixed
- None.

## Step 13 — Build/CI/install hygiene (F17, F18, F20)
**Status**: done
**Date**: 2026-10-07
### What was done
- build.rs: shim compiled with `$RUSTC` (falls back to `rustc`); `debug_core.rs` added to rerun-if-changed.
- Cargo.toml: `rust-version = "1.89"` (File::lock).
- ci.yml: top-level `permissions: contents: read`.
- Pinned by SHA with `# <ref>`: dtolnay/rust-toolchain (`# master`, now with explicit `toolchain: stable`), Swatinem/rust-cache (`# v2`), taiki-e/install-action (`# v2`). actions/checkout stays on its first-party tag.
- New `scripts/bump-action-pins.sh [--check] [files]`: resolves `# <ref>` via `git ls-remote` (peeled tag → tag → branch), rewrites SHAs, `--check` exits 1 when stale. Verified: detects + fixes a zeroed pin in a scratch copy; `--check` is clean on the repo.
- README: install/upgrade use `cargo install --locked … --tag vX.Y.Z`.
- Shim: pass through when the data dir isn't owned by the process's euid (`sudo -E` / root shell with the user's HOME). Test `a_data_dir_owned_by_another_user_disables_interception` (runs only as root, needs chown).
### Deviations from plan
- **Not** the planned `geteuid() == 0` passthrough: tried it and 16 shim tests failed because this environment (like many dev containers) runs everything as root — it would disable the tool for root-only setups. Switched to Security's original alternative (euid ≠ data-dir owner), which still covers the sudo -E case.
- `rust-version = 1.89` not verified with a 1.89 toolchain (not installed here).
### Issues found / fixed
- The bump script caught a wrong hand pin: Swatinem/rust-cache `v2` is an annotated tag; the first pin was the tag object, not the commit. Script uses the peeled commit (6323deb).

## Step 14 — `discover` quoting and test-harness hygiene (F16, §6)
**Status**: done
**Date**: 2026-10-07
### What was done
- `discover::shell_quote`: plain words unchanged, anything else single-quoted (`'` → `'\\''`); applied to every path/name inside the printed `ghostvolumes decide/convert` suggestions. Verified the quoted output round-trips literally through `sh`.
- Test harness: shim/probe/loopback artifacts now go to `CARGO_TARGET_TMPDIR` (under target/) with fixed names instead of `/tmp/...-<pid>` — a full run adds nothing to /tmp.
- Test: `shell_quote_leaves_plain_words_and_neutralises_the_rest`.
### Deviations from plan
- Terminal escape sequences in printed paths stay unescaped (won't-fix, project-wide).
- Old `/tmp/*-<pid>` leftovers from earlier runs (98 files) not deleted.
### Issues found / fixed
- None.

## Step 15 — Milestone review: convert backup fixes (QA-R-1, QA-R-2, QA-R-4, QA-R-6)
**Status**: done
**Date**: 2026-10-07
### What was done
- Kept backup is a wrapper `.<name>.ghostvolumes-convert-old.<unix-secs>[-n]/` holding `.gitignore` (`*`, written only after a successful swap) and the old tree as `<name>/` — git-ignored, and an existing `<name>/.gitignore` is never touched.
- Unique names (`unique_backup_dir`): kept backups never block later converts; only a leftover tmp does. Walk skips names containing `.ghostvolumes-convert-old`. Hint gives the per-backup and glob `rm -rf`.
- Failed rollback error names where the original and the copy are.
- `copy_and_swap_with(path, delete_backup, cp)` — copy program injectable.
- Tests: `only_a_leftover_tmp_blocks_copy_and_swap_and_a_failed_copy_cleans_up` (cp=`false` → tmp removed, original intact; tmp blocks; earlier backup doesn't), `git_never_sees_a_kept_backup` (skips without git), `the_backup_is_kept_by_default_and_deleted_when_configured` updated for the layout incl. the old tree's own .gitignore.
- design.md / CLI help name the new layout.
### Deviations from plan
- Second-rename rollback still untested (needs a hook between renames; agreed deferral).
### Issues found / fixed
- None.

## Step 16 — Milestone review: upgrade path, shim and canonicalization fixes (QA-R-3, SEC-R-4, QA-R-5, SEC-R-3/QA-R-7, Step 8 follow-up)
**Status**: done
**Date**: 2026-10-07
### What was done
- `init` re-runs `reload` when `compiled.tsv` exists; a reload failure is a warning (shim stays installed). Test `rerunning_init_reloads_an_existing_cache_and_tolerates_failure` (symlinked root recompiled to its real path; then a vanished root still lets `init` succeed).
- `intercept` refuses on a missing/stale shim, naming the shim path and the running binary; `shell-init` keeps the warning. CLI test `intercept_refuses_without_a_current_shim_and_runs_after_init`.
- `parse_lines` trims again after the BOM (test input now `BOM + space + '+ a'`).
- `canonicalize_entries`: resolves only ancestors (parent canonicalized + original name); an entry that is itself a symlink is kept with a warning; warns when the rewrite newly nests one project in another. Reload test extended with the self-symlink case.
- `projects register` rejects control characters.
- Shim chmod goes through `/proc/self/fd/<parent fd>/<name>` (anchored to the decided directory).
- README Upgrading: `init` alone completes an upgrade; `intercept` refuses until then.
### Deviations from plan
- No dedicated test for the nesting warning or the register control-char check (stderr-only / 3-line guard).
### Issues found / fixed
- None.

## Step 17 — Final review fixes (SEC-F-1/QA-F-4, SEC-F-2, SEC-F-3, QA-F-1, QA-F-2, QA-F-3, QA-F-6)
**Status**: done
**Date**: 2026-10-07
### What was done
- Shim chmod: `File::open(/proc/self/fd/<parent>/<name>)`, applied via `fchmod` only if the fd is a dir with inode 256; `wanted = (cur & mode & 0o777) | (mode & 0o1000) | (cur & 0o2000)`. Mode test now also checks `01777` under umask 022 → `1755`.
- `projects::register` rejects control characters (unit test `register_rejects_control_characters_from_any_caller`); the main.rs-only check removed.
- `release.yml`: `tool: cargo-release@1.1.6`.
- `init::data_dir_owned_by_euid`; `intercept` refuses when an existing data dir is owned by another user (before the stale-shim check).
- Root-only shim test prints `skipped: …` when unprivileged.
- `copy_and_swap`: successful rollback also removes the temp subvolume.
- ci.yml: `msrv` job (Rust 1.89, `cargo check --locked --all-targets`).
### Deviations from plan
- QA-F-5 (repeated self-symlink warning) deferred.
- MSRV job not run locally (no 1.89 toolchain here) — first CI run will verify.
### Issues found / fixed
- First attempt used a let-chain in shim code; the shim is compiled as edition 2021, so the build script failed — rewritten as nested `if let`.
- The ownership refusal initially fired before `init` (no data dir yet) and masked the 'run init' message — now gated on the data dir existing.

## Step 18 — Shim on edition 2024; keep and document the `/proc` chmod anchor
**Status**: done
**Date**: 2026-10-07
### What was done
- Shim compiled as edition 2024: build.rs, tests/shim_ld_preload.rs (shim + mkdirat probe), tests/btrfs_loopback.rs.
- `#[unsafe(no_mangle)]` on mkdir/mkdirat, `#[unsafe(link_section = ".init_array")]` on the constructor (`#[used]` kept).
- Mode fix-up: let-chain restored; comment now states precisely why it goes through `/proc/self/fd/<parent fd>/<name>` (openat emulation anchoring the decided parent; ino 256 alone can't identify *our* subvolume; no variadic extern / arch O_* values; /proc already required).
- btrfs_core.rs edition comment updated.
- Test `the_shim_exports_mkdir_and_mkdirat` (`nm -D --defined-only`, skips without nm).
- Both toolchains: shim compiles with only the pre-existing `config_dir_from` dead-code warning; 387 passed / 0 failed / 4 ignored on stable and nightly; loopback --ignored passes.
### Deviations from plan
- No separate 'constructor ran' test (Security's optional ask): the cache/log contexts are lazily initialised anyway, so the constructor isn't observable from outside; `#[used]` + link_section unchanged in codegen.
### Issues found / fixed
- None.

## Step 19 — Simplify the chmod to `target.path`; add documents/security.md
**Status**: done
**Date**: 2026-10-07
### What was done
- Shim mode fix-up opens `target.path` (no `/proc/self/fd/<parent>/<name>` anchor); same fd check (dir, inode 256) and fchmod through that fd; comment names the real guard and links security.md.
- New documents/security.md (threat model, defenses table, mkdir-mode window + anchor decision, deliberately-not-defended table); linked from README.
- 387 passed / 0 failed / 4 ignored (stable); shim suite green on nightly.
### Deviations from plan
- Reverses Step 18's 'keep /proc' after the owner's challenge; Security agreed the anchor has no threat model.
### Issues found / fixed
- None.
