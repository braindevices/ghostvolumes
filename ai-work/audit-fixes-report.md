# Audit fixes — final report

**Date**: 2026-10-07 · **Branch**: `claude-audit-fixes`, 19 commits on top of `973f6bd` (17 implementation steps plus plan updates; not pushed, not merged).

| Document | Purpose |
|---|---|
| [`audit-2026-10-06.md`](audit-2026-10-06.md) | Audit report |
| [`tasks/audit-fixes.plan.md`](tasks/audit-fixes.plan.md) | Plan |
| [`tasks/audit-fixes.progress.md`](tasks/audit-fixes.progress.md) | Per-step record: what was done, deviations, issues |

## 1. Result

| | Before (`973f6bd`) | After (`4da3d26`) |
|---|---|---|
| Tests passed / failed / ignored | 348 / 0 / 4 | **386 / 0 / 4** |
| clippy `-D warnings`, fmt | pass | pass |
| Toolchains verified locally | stable 1.98.1 | stable 1.98.1 + nightly 1.100 |
| `cargo test` from an interactive terminal | pass (after the TTY fix) | pass |
| Code diff (src, shim, tests) | — | 20 files, +1603 / −185 |

All **High** and **Medium** findings are fixed, and the most important ones were checked by re-running the original reproductions:

| Finding | Reproduction before | After |
|---|---|---|
| F1: `+ /../x` converts outside the project | `ws/victim` became a subvolume | `..` patterns are dropped by the parser; `check_contained` refuses symlinked or non-directory targets |
| F2: symlinked decision file | append to `~/.bashrc`; OOM on `/dev/zero` | Opened only as a regular file ≤ 1 MiB (lstat, then open, then fstat dev/ino); created with `O_EXCL` |
| F3: BOM aborts the host process | exit 134 | Parser can't panic; the shim test passes |
| F4: `init` crashes running shells (SIGBUS) | 2 of 3 runs crashed | 200× `init` during a preloaded `mkdir` loop: no crash |
| F5: `convert` loses concurrent writes | 24,299 files lost | 57,070 of 57,070 writes kept (new subvolume plus kept backup) |
| F8/F9: EEXIST success, `mode` ignored | `mkdir` on a file returned 0; mode 0755 | EEXIST from the real `mkdir`; `mode & ~umask` (setgid and sticky kept) |
| F10/F12/F21 and others | — | Fixed; see §3 |

## 2. Process

1. **Steps 1–7 (P0):** implemented, then reviewed in parallel by the QA and Security agents (2 Medium and several Low findings). One debate round followed; Security corrected the backup `.gitignore` design and QA won its case for the copy-failure test. The result became Steps 15–16.
2. **Steps 8–16:** implemented, then the final review: nothing High or Medium, 9 Low/Info. Both reviewers agreed on the fixes, which became Step 17.
3. Each step was implemented, tested (including checking that new tests fail against the old code where possible) and committed on its own; `cargo fmt` and clippy ran every time.

## 3. What changed, by step

| Step | Change |
|---|---|
| 1 | Parser can't panic; `valid_pattern` rejects `.`/`..` and control characters (decision files, markers, `decide --add/--deny`) |
| 2 | `init` installs the shim atomically; stale-shim detection |
| 3 | `open_regular_file`/`read_regular_file` for every decision/ignore read and append |
| 4 | `check_contained` guard in `materialize`; CLI paths canonicalized (resolved by the kernel, not lexically) |
| 5 | **Shim redesign:** watched-name filter first (no syscalls); parent opened and its real path read from `/proc/self/fd`; subvolume created on the same fd; `reload` writes physical paths |
| 6 | Docs: the lock's real scope, containment, intercept example |
| 7 | `convert` keeps the reflinked backup by default; `delete-convert-backup` config key and `--delete-backup` flag; rollback and cleanup |
| 8 | Shim: real EEXIST; `mode` honoured |
| 9 | Pending markers only inside registered projects |
| 10 | Shim log opened per line (no held fd, no mutex) |
| 11 | `intercept` appends to an existing `LD_PRELOAD`; signal deaths return 128+N |
| 12 | `mountinfo` keeps non-ASCII mountpoints intact |
| 13 | Third-party actions SHA-pinned plus `scripts/bump-action-pins.sh`; CI token read-only; `$RUSTC`; `rust-version`; `--locked` installs; shim skips when the data dir has another owner |
| 14 | `discover` shell-quotes the commands it suggests; test artifacts moved out of `/tmp` |
| 15 | Backups go in a git-ignored wrapper dir with a unique name; failed-copy test (copy program injectable) |
| 16 | `init` re-runs `reload` (an upgrade is complete after `init` alone); `intercept` refuses a stale shim; `canonicalize_entries` resolves only ancestors |
| 17 | Mode applied via a verified fd; `register` rejects control characters; `cargo-release@1.1.6` pinned; ownership refusal; rollback cleanup; MSRV CI job |

The owner's decisions are recorded in the plan: Q1 keep the backup (reflinked, delete is opt-in), Q2 no, Q3 no, F4 fixed in code and docs, `..` handled per input.

## 4. Deviations you should know about

- **Step 13: no `euid == 0` passthrough.** It broke every shim test, because this environment, like many dev containers, runs as root, so it would switch the tool off for those users. The shim now skips only when the data dir isn't owned by the process's euid; that covers `sudo -E`. QA and Security both agreed.
- **Backup layout changed twice.** First a plain `.<name>.ghostvolumes-convert-old`; after the review it became `.<name>.ghostvolumes-convert-old.<secs>[-n]/{.gitignore,<name>/}`, so git ignores it, existing `.gitignore` files are never overwritten, and a kept backup never blocks a later convert.
- **Behaviour changes for users:**
  - `convert` now leaves a backup (and prints an `rm -rf` hint).
  - `intercept` refuses to run until `init` after an upgrade.
  - The shim's `?` markers appear only in registered projects.
  - `projects register` needs an existing directory.
- **The release should come after Step 7, not after Step 6** (the docs describe the kept backup). That point is passed now.

## 5. Not verified / open

- **`rust-version = "1.89"`:** there is no 1.89 toolchain here. The new CI `msrv` job will check it on its first run.
- **New CI configuration** (pinned SHAs, `msrv` job, `toolchain: stable` input): YAML structure checked by eye (no YAML parser available here), and `bump-action-pins.sh --check` passes. Not run on GitHub yet.
- **Untested branches:** the rollback after a failed second rename (needs a hook between the two renames), and `register`'s control-character check from the `convert` path (unit-tested in `projects::register` itself).
- **Root-only test:** `a_data_dir_owned_by_another_user_disables_interception` runs only as root (locally yes, in CI it skips).
- **Deferred:**
  - QA-F-5: the self-symlink warning repeats on every reload.
  - SEC-R-1: a FIFO swapped in during check-then-open needs an active racer, and the flag values differ by architecture.
  - SEC-R-2: hardlinks are created by legitimate tools.
  - SEC-F-4/5/6: Info.
- **Won't fix:** as listed in the audit report, §3.

## 6. Side effect outside the repo

While testing as uid 65534, the QA agent ran `chmod a+rx /root` and afterwards set it to `0700`. `/root` is now **`2700` (`drwx--S---`, root:nobody)**. The setgid bit matches the other directories in this container, so it was probably there before; whether group or others originally had access is unknown. **Please check and restore it if needed.**

Old `/tmp/*-<pid>` test artifacts from earlier runs (98 files) are still there; the tests no longer create them.

## 7. Next steps (yours)

1. Review this branch, then merge `claude-audit-fixes`; it contains `claude-audit-review` and the CI fixes.
2. Push: CI runs the `msrv` job and the pinned actions for the first time.
3. Cut the release once CI is green.
