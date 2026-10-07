# Audit fixes — plan

Source: [`../audit-2026-10-06.md`](../audit-2026-10-06.md). Branch: one `claude-audit-fixes` branch, one commit per step (`Step N: …`).

Each step follows **implement → test → fix**, with `cargo fmt` + `cargo clippy --all-targets -D warnings` before every commit. Shim changes are tested through `tests/shim_ld_preload.rs` on the real BTRFS scratch dir (`btrfs_scratch_dir()`).

Steps 1–6 are **P0**. Cut the next alpha release after step 6.

## Owner decisions (2026-10-07)

- **Q1: keep the backup by default (step 7).** After a swap, `convert` keeps `.<name>.ghostvolumes-convert-old` and prints its path, which closes F5.
  - The copy is already `cp -a --reflink=always` (`convert.rs:359-361`), so the backup shares extents with the new subvolume and costs no extra space until files diverge.
  - The `--reflink=always` flag stays as it is, so a copy that can't be reflinked fails rather than silently doubling disk use.
  - To delete it automatically instead: set the config key `delete-convert-backup = true` (top-level, last file wins, like `default-ignore`), or pass `convert --delete-backup` for one run.
- **Q2: no.** Committed `+` lines for names that aren't watched get no extra prompt.
- **Q3: no.** No `catch_unwind` safety net in the shim.
- **F4: fix the code and the docs (QA and Security agree).**
  - The README isn't wrong: `cargo install --force` replaces only the binary, and only `init` copies the embedded shim to `$XDG_DATA_HOME`.
  - The danger is how `init` writes it (truncate in place), so fix that in code (step 2). A doc-only rule is one users will forget.
  - Also, a shim left stale after an upgrade disagrees with the new CLI about file formats and lock paths, and misses security fixes. Step 2 therefore adds a staleness warning.
- **`..` support, decided per input (QA and Security agree):**
  - **Decision-file patterns: reject.** Any component that isn't a normal name makes the line invalid (step 1). Nobody needs it, and lexical normalisation would be wrong (`a/link/..` is not `a` in the kernel).
  - **CLI path arguments: canonicalize, don't reject.** `convert ../proj` is normal input. Canonicalizing stores the physical path, which is what the shim sees through `getcwd()`.
    - Paths that may not exist yet (`convert`, `--create`): canonicalize the deepest existing ancestor and append the rest; `..` in that rest is rejected.
    - `projects register`: the path must exist.
    - Moved into step 4.
  - **Shim mkdir targets: resolved by the kernel, not by text (step 5).** The shim opens the target's parent and reads its real path back from `/proc/self/fd`, so symlinks and `..` resolve the way the kernel resolves them.
    - The last component of a successful `mkdir` is never `.` or `..`, so that real parent path plus the name is the real target.
    - Verified 2026-10-07: `other/link/a/..` resolves to `…/real`, not to `other/link`; a deleted directory reads back with a ` (deleted)` suffix.

## Steps

### Step 1 — Parser cannot panic; one pattern-validity predicate (F3, F6, N5) · P0
Files: `shim/decision_core.rs`, `src/decision.rs` (`decide --add/--deny`).
- In `parse_lines`: strip a leading BOM, and replace `split_at(1)` with `strip_prefix('+')` / `strip_prefix('-')`.
- Add `valid_pattern(&str) -> bool`, which rejects `.` / `..` / empty components and any control character.
  - `parse_lines` and `parse_anchored_exact_patterns` skip invalid patterns.
  - `decide --add/--deny` returns an error on them.
- `anchored_pattern` (marker builder) returns `None` when the path contains a control character.
  - The shim's append and the CLI's append both skip it.
  - The two `unwrap_or_else(display)` fallbacks at `convert.rs:277/614` are changed to skip.

Tests:
- `parse_lines("\u{feff}+ a\né b\n+ /../x\n+ z\n")` yields `a` and `z` only.
- `anchored_pattern` of a path containing `\n` returns `None`.
- `decide --add ../x` returns an error.
- Shim integration test: a BOM-prefixed decision file plus `mkdir` returns normally (no abort).

### Step 2 — Atomic shim install, plus a stale-shim warning (F4) · P0
Files: `src/atomic_write.rs`, `src/init.rs`, `src/main.rs` (`intercept` and `shell-init` arms), README "Upgrading"/Install.
- `write_atomically` takes `impl AsRef<[u8]>`.
- `init` installs the `.so` through it (temp file, then rename). Running processes keep the old inode, so no SIGBUS.
- Add `init::shim_is_current(data_dir) -> bool`: compare the installed `.so` with the embedded `PRELOAD_SO`, size first, then bytes.
  - `intercept` and `shell-init` print a warning on stderr when it returns false: "shim out of date — run `ghostvolumes init`".
- README changes:
  - Fix the claim that "init compiles the shim".
  - Add to Upgrading: shells and sessions already running keep the old shim until they restart.

Tests:
- The inode of the installed `.so` changes across two `init` runs, and its contents are identical.
- `shim_is_current` returns false after the installed file is changed and true after `init`.

### Step 3 — Safe decision/ignore-file I/O (F2) · P0
Files: `shim/decision_core.rs` (one helper, shared by the CLI and the shim). Call sites:
- `shim/preload.rs` (read and append);
- `src/convert.rs` (read, append, `record_decision`, anchored candidates, ignore file);
- `src/discover.rs`, `src/completions.rs`, `src/intercept.rs`.

The helper, `open_decision_file(path, append)`:
1. `symlink_metadata` must show a regular file, or the path must be absent (when appending).
2. Open the file.
3. `metadata()` on the open fd must have the same dev/ino as step 1, and be ≤ 1 MiB.
4. Otherwise it returns `None`: reads see an empty file, appends are refused and logged.
- `record_decision` refuses to rewrite a symlinked file.

Tests:
- A symlink to a temp file: reads return nothing, appends are refused, the target is unchanged.
- A symlink to `/dev/zero` returns promptly.
- A FIFO is rejected without blocking.
- A directory named `.ghostvolumes-decisions` is skipped.

### Step 4 — Containment guard for `convert` mutations; `..` handling (F1, F13, real-dir part of F7) · P0
Files: `src/convert.rs` (`materialize`, `create_empty`), `src/main.rs` (`absolutize`). (Shim-side path resolution is step 5.)
- `absolutize` canonicalizes the deepest existing ancestor and appends the rest. A `..` in that rest is an error. `projects register` requires the path to exist.

A single guard in `materialize`, run before `create_dir_all` / `copy_and_swap`. It requires:
- `target.strip_prefix(boundary)` succeeds and every component is `Normal`;
- no existing component from the boundary down to the target is a symlink;
- if the target exists, it is a real directory.

Tests (`convert_with_io`), each checking that the outside directory, the link target or the file is untouched:
- `+ /../victim`;
- `+ /link/sub` with the link pointing outside the project;
- `+ /afile` where `afile` is a regular file;
- `--create` with the same escape patterns.
- `projects register ../proj` stores the canonical path.
- `convert --create ../x/new` is rejected.

### Step 5 — Shim resolves targets by opened parent fd (F10, `..`, N3/F13 shim side, decide→create race) · P0
Files: `shim/preload.rs` (`mkdir`, `mkdirat`, `resolve_path`, `dirfd_path`, `decide`, `try_create_subvolume`), `shim/btrfs_core.rs`, `shim/cache_core.rs`, `src/reload.rs`.

New flow for each intercepted call:
1. **Name first, no syscalls.** Take the last component of the raw path.
   - Pass through if it is empty, `.` or `..`, not UTF-8, or not in the union of watched names in `compiled.tsv` (a set built once with the cache).
   - This drops today's `getcwd()` before the name check, so the common case gets faster.
2. **Open the parent.**
   - `mkdir`: `dirname(raw)` relative to the cwd (the kernel resolves it; no `getcwd`).
   - `mkdirat`: `/proc/self/fd/<dirfd>/<dirname>`, or the cwd form for `AT_FDCWD`.
   - Run the `is_dir` guard before opening (no FIFO block), then `std::fs::File::open`.
   - Pass through on any failure: a bad dirfd, a missing parent, or a write-only parent (opening needs read permission; `mkdir` does not).
3. **Get the real parent path:** `read_link("/proc/self/fd/<parent fd>")`. Pass through if it fails (no `/proc`) or ends in ` (deleted)`.
4. **Decide** on `real_parent.join(name)`: cache root match, `is_subvolume`, boundary walk-up and decision files, all on real paths.
5. **Create** with the ioctl on the **same parent fd**.
   - `btrfs_core::create_subvolume` takes the open `&File` and a name. The CLI's `convert` callers open the parent themselves.
   - A symlink swapped in after the decision can't redirect the creation.

Supporting changes:
- `dirfd_path`'s silent fallback to the cwd is removed (fixes F10).
- `reload` writes canonical roots into `compiled.tsv` and canonicalizes existing `project-roots.list` entries, so every path the shim compares against is real.
  - Mount roots from mountinfo already are.
  - New registrations are canonicalized in step 4.
  - Old entries written with a symlinked spelling (e.g. `/home` → `/var/home`) get fixed by this one-time pass.
- The mkdir semantics in step 8 (EEXIST, mode) build on this flow.

Tests (`tests/shim_ld_preload.rs`, real BTRFS scratch, `+ node_modules`):
- `mkdir("link/node_modules")` where `link` points to a directory inside the project creates a subvolume at the real path, and the decision is read from the real path's decision file.
- `mkdir("a/../node_modules")` creates a subvolume at the real `<proj>/node_modules`.
- `mkdir("link/../node_modules")` with `link` pointing outside the project resolves the way the kernel does (outside), so the outside decision rules apply: no `+` there means a plain directory.
- `mkdirat` with an invalid dirfd returns -1 with EBADF.
- `mkdirat` relative to a directory fd works.
- `mkdir("node_modules")` with a deleted cwd passes through (ENOENT from the real call).
- An unwatched name never opens anything. Check with `strace -c` if available; otherwise a debug-log assertion that `ENTER` isn't logged.

### Step 6 — P0 doc corrections (F5 claim, F22) · P0
Files: `documents/design.md` (~L209-217 lock claim), `documents/convert.md` (scope claim), `documents/intercept.md` (example and `? /build` format), README ("shim never writes stderr" if present).

Docs only. No test needed; check the rendered TOC with `scripts/update-toc.sh`.

### Step 7 — `copy_and_swap` hardening (F5 code, F7, F15) · P1
Files: `src/convert.rs:338-411`.
- Pass `cp` its source as `path.join(".")`, an `OsStr`, not a lossy `display()` string.
- If `cp` fails, remove the temp subvolume (best effort).
- If the second rename fails, rename the backup back.
- Backup handling follows **Q1**:
  - Keep `.<name>.ghostvolumes-convert-old` by default and print `backup kept: <path>`.
  - Delete it only when `delete-convert-backup = true` is set (`MergedConfig`, `init`'s defaults TOML) or `--delete-backup` is passed.
- A failed cleanup after the swap is a warning, not an `Err`, so the `+` still gets recorded.
- The walk skips `*.ghostvolumes-convert-{tmp,old}`.

Tests:
- A forced `cp` failure leaves no temp dir and no `?` line.
- A leftover `-old` dir is not reported as a candidate.
- By default, the backup exists after the swap and its path is printed.
- With `--delete-backup`, or with the config key set, no backup is left.

### Step 8 — Shim mkdir semantics (F8, F9) · P1
F10 moved to step 5. F21 is already fixed on `main` (`98840f7`, `3526f18`, `bd448cf`): the parent is opened with `std::fs::File` behind an `is_dir` guard, and `ioctl` is declared variadic.
Files: `shim/preload.rs`, `shim/btrfs_core.rs`.
- On EEXIST, fall through to the real `mkdir`, which sets the correct errno.
- Only on the `Created` path: `set_permissions(cur_mode & mode & 0o7777)`.

Tests, under `+`:
- `mkdir` on an existing regular file → -1 with EEXIST;
- `mkdir` on an existing plain dir → EEXIST;
- `mkdir(…, 0700)` → mode 0700;

### Step 9 — No pending markers outside registered projects (F11) · P1
Files: `shim/preload.rs` (Undecided arm), and the matching CLI path if there is one.
- Append the marker only when the boundary is a registered project root; always log.

Test: `mkdir` in an unregistered project leaves no volume-root decision file, and `convert` then works without the orphan abort.

### Step 10 — Log opened per line (F12) · P2
Files: `shim/preload.rs`.
- Remove the fd opened in the constructor and its Mutex.
- Each log line opens the file with `O_APPEND`, writes once, and closes.

Test: QA's `closerange` + `open` repro; the victim file stays unchanged.

### Step 11 — `intercept` correctness (F14) · P2
Files: `src/intercept.rs`.
- Append to an existing `LD_PRELOAD`, as shell-init does.
- Return `128+signal` for signal deaths.
- Fix the existing test so it asserts that `LD_PRELOAD` was set.

Test: the child's environment contains both preload entries.

### Step 12 — mountinfo bytes (F19) · P2
Files: `src/mountinfo.rs`. (The F13 path normalisation moved to steps 4 and 5.)
- Unescape into a `Vec<u8>`, then `from_utf8_lossy`.

Test: `unescape("/mnt/donn\\303\\251es")` round-trips.

### Step 13 — Build/CI/install hygiene (F17, F18, F20) · P2
Files:
- `build.rs`: `$RUSTC`; add `debug_core.rs` to the rerun list.
- `Cargo.toml`: `rust-version = "1.89"`.
- `ci.yml`: `permissions: contents: read`.
- Workflows: pin `dtolnay/rust-toolchain`, `taiki-e/install-action` and `Swatinem/rust-cache` by SHA, written as `uses: owner/repo@<sha> # <ref>`.
  - `dtolnay/rust-toolchain` picks its toolchain from the ref name. Once pinned to a SHA, every use must say `with: toolchain: stable` explicitly.
- New `scripts/bump-action-pins.sh`, in the same style as `scripts/update-toc.sh`:
  - For each `uses: owner/repo@<sha> # <ref>` line in `.github/workflows/*.yml`, resolve `<ref>` with `git ls-remote https://github.com/owner/repo <ref>`.
  - Rewrite the SHA in place and print `owner/repo: old → new`.
  - Lines without a `# <ref>` comment, and local actions, are left alone.
  - `--check` makes no changes and exits 1 if any pin is out of date.
  - Needs only `git`, `sed` and `grep`; no GitHub token.
  - Alternative with no code: Dependabot's `github-actions` ecosystem keeps `@<sha> # <tag>` pins updated through PRs. The script is what the owner asked for.
- README: `cargo install --locked --git … --tag vX`.
- `shim/preload.rs`: `geteuid()==0` passthrough.

Tests:
- CI green.
- One shim test checks the euid guard by unit-testing the predicate (root can't be faked).
- `scripts/bump-action-pins.sh --check` exits 0 right after a bump.
- With one pin rewritten to an old SHA in a scratch copy of a workflow, the script detects and fixes it.

### Step 14 — `discover` quoting and test-harness hygiene (F16, §6) · P2
Files: `src/discover.rs:290-330`, `tests/shim_ld_preload.rs`.
- Single-quote paths in `discover`'s printed commands.
- Remove the `/tmp` shim `.so` and probe files after each test.

Test: a path containing `'` and `$` prints a command that is safe to paste.

### Step 15 — Milestone review: convert backup fixes (QA-R-1, QA-R-2, QA-R-4, QA-R-6) · P1
Added after the QA/Security review of steps 1–7 and one debate round (2026-10-07).
Files: `src/convert.rs`.
- **Backup layout:** the kept backup becomes a wrapper dir, `.<name>.ghostvolumes-convert-old.<unix-secs>[-n]/`. It holds `.gitignore` (`*`) and the old tree as `<name>/`.
  - git ignores the whole backup.
  - A `.gitignore` already inside the old tree is never overwritten.
  - The `.gitignore` is written only after the swap succeeds.
- **Unique names:** an existing backup no longer blocks the next convert; only a leftover tmp does. The walk skips any name containing `.ghostvolumes-convert-old`. The hint suggests a glob for cleanup.
- If the rollback rename fails too, the error names where the data is now (backup and tmp paths).
- `copy_and_swap` takes the copy program as a parameter, so a test can pass `false` to check cleanup after a failed copy.

### Step 16 — Milestone review: upgrade path, shim and canonicalization fixes (QA-R-3, SEC-R-4, QA-R-5, SEC-R-3/QA-R-7, Step 8 follow-up) · P1
Files: `src/init.rs`, `src/main.rs`, `README.md`, `shim/decision_core.rs`, `shim/preload.rs`, `src/projects.rs`.
- `init` runs `reload` when `compiled.tsv` already exists. A failure is a warning, never an `init` failure. README Upgrading says that `init` alone completes an upgrade.
- `intercept` refuses on a missing or stale shim, naming both paths; `shell-init` keeps the warning.
- `parse_lines` trims again after stripping the BOM.
- `canonicalize_entries`:
  - An entry whose own last component is a symlink is kept as written, with a warning: the shim matches real paths, so that entry won't match.
  - Only symlinked ancestors are rewritten.
  - A warning is printed when the rewrite newly nests one registered project inside another.
- `projects register` rejects control characters.
- Shim mode fix (step 8): chmod through `/proc/self/fd/<parent fd>/<name>`, so it is anchored to the decided directory rather than a re-resolved path.
- Debated and won't fix:
  - SEC-R-1 (FIFO swap race): needs an active racer, and the flag values differ by architecture.
  - SEC-R-2 (hardlinks): legitimate tools create hardlinks.
  - QA-R-5 warning for ignored lines: deferred (YAGNI).
  - QA-R-6 second-rename rollback test: needs a hook between the two renames.

### Step 17 — Final review fixes (SEC-F-1/QA-F-4, SEC-F-2, SEC-F-3, QA-F-1, QA-F-2, QA-F-3, QA-F-6) · P1
Added after the final QA/Security review of steps 8–16 (2026-10-07). All Low; all agreed by both reviewers.
- Shim mode fix-up:
  - goes through an fd opened via the parent, and only if that fd is the new subvolume (directory with inode 256);
  - keeps an inherited setgid bit and a requested sticky bit.
- The control-character check moves into `projects::register`, so `convert`/`decide` auto-registration is covered too.
- `release.yml` pins `cargo-release@1.1.6`.
- `intercept` refuses when an existing data dir belongs to another user (the shim would silently pass every call through).
- The root-only shim test prints a note when it skips.
- After a successful rollback, the temp subvolume is removed.
- CI `msrv` job: `cargo check --locked --all-targets` on Rust 1.89.
- Deferred: QA-F-5 (the self-symlink warning repeats on every reload; it only fires while the anomaly exists), SEC-F-4/5/6 (Info).

### Step 18 — Shim on edition 2024; keep and document the `/proc` chmod anchor · P2
Owner request (2026-10-07), debated with Security.
- **Simplify the chmod to `target.path`: rejected.** Every subvolume root is inode 256, so the fd check can't tell ours from another one, and only the parent-fd anchor stops an ancestor swap from redirecting the chmod. The `/proc` path stays, with a comment explaining why.
- **Edition 2024 for the shim: agreed**, with no safety-relevant change: the lock and the parent `File` are named bindings, there is no `static mut`, and the attributes only change syntax.
- Changes: `--edition 2024` in `build.rs` and in the test `rustc` calls; `#[unsafe(no_mangle)]` and `#[unsafe(link_section)]`; the nested `if let` becomes a let-chain again; a new test checks the `nm -D` exports.

### Step 19 — Simplify the chmod to `target.path`; add documents/security.md · P2
Owner challenge, debated with Security (2026-10-07). Security withdrew its objection to Step 18's `/proc` anchor.
- Redirecting the chmod needs write access to an ancestor, which already lets an attacker replace the whole project. A sticky-bit ancestor blocks the rename outright.
- chmod follows symlinks; the guard is the `is_dir && ino == 256` check on the opened fd, with `fchmod` through that same fd.
- New `documents/security.md`: threat model, defenses, the `mkdir` mode window and why the anchor isn't needed, and what is deliberately not defended.

## Out of scope / won't fix
See §3 of the audit report.
