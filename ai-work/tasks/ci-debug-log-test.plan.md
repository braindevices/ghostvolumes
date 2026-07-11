# Debug `debug_mode_logs_every_decision_with_its_reason` failing on ubuntu-26.04

## Problem

`tests/shim_ld_preload.rs`'s `debug_mode_logs_every_decision_with_its_reason`
failed in the `test` job's `ubuntu-26.04` leg only:

```
assertion failed: log_text.contains("-> SKIP (already a subvolume)")
```

Confirmed facts:
- **ubuntu-24.04 passes.** Reproduced locally (this sandbox's real BTRFS
  mount) 20+ times in a row, including the whole `shim_ld_preload` suite
  back-to-back — never once flaky here.
- **ubuntu-26.04 fails**, per the pasted CI log. That leg is already
  `continue-on-error: true` (it's the preview/pre-GA runner image, gated
  non-blocking for that reason already — see `ci.plan.md`), so this
  failure does **not** block merges today.
- The test does three sequential `mkdir` subprocess calls against the
  same target path (`scratch/node_modules`):
  1. create fresh → expects `-> ACCEPT (created subvolume)` — **present** in
     the failing log.
  2. unrelated name, no cache match → expects `-> SKIP (no cache match)` —
     **present**.
  3. re-run on the now-existing subvolume → expects
     `-> SKIP (already a subvolume)` — **missing**. This call's exit status
     isn't asserted (comment: "not asserted, only the log matters here").

So decision 1 and 2 both logged correctly; only decision 3, re-checking a
path this exact test just created two steps earlier, didn't log the
expected line.

## Root-cause hypotheses (ranked)

`decide()` (`shim/preload.rs:210`) reaches `AlreadySubvolume` only via
`btrfs_core::is_subvolume(target)` returning `Ok(true)` — i.e. `stat()`
succeeds and `ino() == 256`. For that branch to be skipped, either the
`stat()` failed (unlikely — nothing removes the directory between calls)
or it succeeded but `ino() != 256` at that moment.

1. **Most likely: a creation/visibility race specific to 26.04's runner
   environment.** If `stat()` in the third (fresh) process transiently
   returns `ENOENT` right after the second process exited — e.g. slower
   or differently-cached storage under whatever 26.04's preview runner
   uses for the loop-mounted image — `is_subvolume` would return `Err`,
   `.unwrap_or(false)` treats that as "not a subvolume," and `decide()`
   falls through past `GitTracked` straight to `Accept`. `try_create_subvolume`
   then hits real `EEXIST` from the kernel (the subvolume genuinely exists)
   and *maps that to success* (`ErrorKind::AlreadyExists => true`, by
   design — plan §5 point 7, for legitimate concurrent-mkdir races), so it
   logs `-> ACCEPT (created subvolume)` a second time instead of the
   expected `SKIP` line. This fits every observed fact: `ACCEPT` is present
   (just written twice, and the test only checks `contains`, not count),
   `SKIP (already a subvolume)` never appears, and the third call's exit
   code — which would still reflect the real underlying `mkdir()`
   returning `EEXIST` since `try_create_subvolume`'s success doesn't
   correspond to the actual libc `mkdir()` return value here — was never
   asserted, so nothing else caught it.
2. **Less likely: a glibc/coreutils symbol-resolution difference on
   26.04's newer toolchain** that causes the third `mkdir` invocation to
   bypass our intercepted `mkdir`/`mkdirat` symbols entirely for the
   already-exists case specifically. Weaker fit: this would produce *no*
   log line at all for that call (not a wrong one), which is consistent
   with what we see, but doesn't explain why only the *third* call would
   differ from the *first* — the same binary makes all three calls via
   the same code path each time.

## Real evidence (downloaded job log, `7_cargo test.txt`)

The actual raw log (not just the trimmed failure text) confirms:
- **Real, full test-level concurrency**: 13 `shim_ld_preload` tests ran in
  parallel threads (cargo test's default is one thread per core) and
  finished in 0.51s total; output from different tests is visibly
  interleaved.
- Exactly two `mkdir: ...: File exists` lines appear in the whole log,
  and both are accounted for by the two tests that legitimately do a
  second, unasserted `mkdir` on an already-created subvolume
  (`debug_mode_logs_every_decision_with_its_reason` and
  `mkdir_on_an_already_existing_subvolume_...`) — ruling out cross-test
  path collision as the cause (each test's target is already under its
  own unique `tempdir_in`-generated subdirectory).
- `ACCEPT (created subvolume)` and `SKIP (no cache match)` both logged
  correctly; only `SKIP (already a subvolume)` is missing for the third
  call.

Since `BTRFS_IOC_SUBVOL_CREATE` is a synchronous ioctl — the calling
process doesn't return from it until the kernel has fully committed the
new subvolume — hypothesis 1 above (a visibility-latency race on a
`stat()` from a *different* process) doesn't actually hold up mechanically
on a local, single-node filesystem. What the raw log's interleaving does
show, though, is genuine, heavy concurrent load: dozens of test processes
were doing real `BTRFS_IOC_SUBVOL_CREATE`/`stat()` calls against the same
underlying filesystem at once. BTRFS serializes subvolume creation with a
filesystem-wide (not per-directory) lock in the kernel, so this is real
contention, not just CPU noise — and it's the most plausible source of
whatever transient hiccup produced the wrong decision branch here, even
without a fully proven step-by-step mechanism.

## First fix attempt: `--test-threads=1` (insufficient)

Forced the whole suite to run single-threaded in CI. **This did not fix
it** — the exact same failure reproduced on a subsequent 26.04 run, fully
serialized, with no other test running concurrently at any point. This
conclusively rules out cross-test/cross-thread contention (including the
BTRFS-subvolume-creation-lock theory above) as the cause: something
about this one test misbehaves even in complete isolation on this
specific environment.

## Real finding: the shim recursively loads itself via its own git check

Added instrumentation (raw `is_subvolume()` result logging in
`decide()`, `tree`/`stat` dumps, and — critically — an unconditional
side-channel diagnostic in `load_log_context()` recording `argv0`,
`GHOSTVOLUMES_DEBUG`, the resolved log path, and whether opening it
succeeded, for *every* process that loads the shim, not just ones that
call `mkdir`/`mkdirat`).

Running the test locally under this instrumentation surfaced a real bug
immediately: for a 3-call test, **4 processes** loaded the shim, and the
4th had `argv0="git"`. `decide()` (`shim/preload.rs`) calls
`git_core::is_git_tracked(target)` whenever a path matches the cache but
isn't already a subvolume — which is exactly what happens on the *first*
call of this test (target doesn't exist yet, so `is_subvolume` returns
`Err(NotFound)`, falls through to the git check). That call
(`shim/git_core.rs`) shells out to `git ls-files` via `Command::new()`
**without stripping `LD_PRELOAD`** from the child's environment. Since
`LD_PRELOAD` is set on the `mkdir` process specifically so the shim can
intercept it, and child processes inherit environment variables by
default, `git` inherits it too — and being a normal dynamically-linked
binary, its own dynamic linker loads our shim and runs its constructor,
completely unrelated to what `git ls-files` was invoked to check.

Fixed: `git_core.rs`'s `Command` now calls `.env_remove("LD_PRELOAD")`.
Verified locally: before the fix, 4 diagnostic entries for 3 `mkdir`
calls; after, exactly 3. Since `git_core.rs` is `include!()`-shared with
`src/git.rs`, this also fixes the main CLI's own git-tracked check the
same way.

This is a real, independently-valuable bug regardless of whether it's
*the* cause of the 26.04 flake — recursively self-loading on every
git-tracked check means every mkdir call that reaches that check spawns
an extra process that redundantly re-reads the cache, tries to open the
log file again, etc. That's real added process/fd churn on every such
call, which is a very plausible contributor to a flake that only
surfaces on a more resource-constrained preview runner, even without
100% proof it was *the* mechanism behind the specific missing log line
observed so far (we have never reproduced the original failure locally,
on any environment — only confirmed and fixed the recursion itself).

## Update: `git_core.rs` fix confirmed real, but the flake persists

Next real CI run (`logs_78826269942`), with `--show-output` so both
24.04 (passing) and 26.04 (failing) show full output: the recursive
`git` self-load is confirmed gone on 26.04 too — the side-channel diag
file now shows exactly 3 entries, one per `mkdir` call
(`argv0="/usr/bin/coreutils"` each time, no more `argv0="git"`). **The
test still fails the same way.**

Crucially, the diag entry for the third call (pid 3134) shows
`GHOSTVOLUMES_DEBUG=Ok("1") debug=true log_path=Some("/tmp/.tmpO0VheR")
open_result=Some(Ok)` — proving `load_log_context()` ran successfully
for that process: it read the debug flag correctly, resolved the same
log path as the other two calls, and opened it successfully. Yet the
main log file has zero lines for that pid. Since `load_log_context()`
(the constructor) definitely ran and definitely succeeded, but
`decide()`'s logging never fired, the remaining explanation is that our
exported `mkdir`/`mkdirat` symbols were never entered at all for that
one process — something about the *third* invocation specifically
doesn't route through either intercepted symbol, despite being the
exact same `mkdir <path>` command line as calls 1 and 2.

Added one more diagnostic to test this directly: `diag_entry()`, called
unconditionally at the very top of both exported `mkdir()`/`mkdirat()`,
before any other logic (bypassing `log_debug`/the debug flag/cache load
entirely) — so the next run will show definitively whether our symbols
were entered at all for every call, not just infer it from an absence
of decision logs.

## Root cause (confirmed)

The next CI run's diagnostics were unambiguous: the third call's diag
entry showed `load_log_context()` ran and succeeded (constructor fired,
debug flag read correctly, log file opened fine) but **no
`ENTERED mkdir`/`ENTERED mkdirat` line at all** — proving our exported
symbols were never called for that invocation, even though the shim
itself was loaded into the process.

Checked what's actually installed as `/usr/bin/mkdir` on each image via
the Ubuntu package archive: Ubuntu has been transitioning its default
`coreutils` package to a Rust reimplementation (the `uutils/coreutils`
project, packaged as `rust-coreutils`).
- **ubuntu-24.04**: `coreutils` = classic GNU coreutils 9.4.
- **ubuntu-26.04 (devel)**: the default `coreutils` package is
  `rust-coreutils` 0.9.0; GNU's implementation still exists but has been
  renamed to `gnu-coreutils`, no longer installed by default.

Pulled both implementations' actual source:
- GNU coreutils (gnulib's `mkdir-p.c`/`mkancesdirs.c`): for a plain
  `mkdir <path>` (no `-p`), always calls `mkdir()` unconditionally first
  and only inspects `errno` afterward — no existence pre-check, ever.
- uutils' `mkdir` (`src/uu/mkdir/src/mkdir.rs`):
  ```rust
  let path_exists = path.exists();
  if path_exists && !config.recursive {
      return Err(USimpleError::new(..., "mkdir-error-file-exists", ...));
  }
  ```
  It calls `path.exists()` (→ `stat()`/`lstat()`, symbols this shim
  never intercepts) *before* attempting creation, and short-circuits
  immediately when the target already exists — `create_dir()` (and
  therefore libc `mkdir()`/`mkdirat()`) is **never called** in that case.

This is fully deterministic per environment, not a race: run 1 (fresh
target) always reaches real creation under both implementations and
gets intercepted correctly either way. Only "redirect an already-existing
target back through mkdir()/mkdirat()" differs, and only uutils skips it
— which is a real, legitimate difference between `mkdir` implementations,
not a bug in the shim. The shim's actual job (convert fresh matching
directories into subvolumes) is unaffected either way.

## Resolution

Rather than replace the real system `mkdir` in the test with a synthetic
probe (would test something other than actual OS behavior), accommodated
the difference directly:

1. **`shim/preload.rs`**: added a permanent, debug-gated `-> ENTER` log
   line at the top of `handle_intercept()` (before `decide()` runs), so
   the normal debug log can always distinguish "the shim was entered but
   decided X" from "the shim was never entered for this call." Reverted
   all the temporary unconditional/side-channel diagnostics
   (`diag_entry()`, the argv0/open-result dump in `load_log_context()`)
   now that the mechanism is understood — they added per-call overhead
   and `/tmp` pollution that don't belong in shipped code. Kept the raw
   `is_subvolume()` result logging in `decide()` (`ai-work` §8.5's
   "why accept, why ignore" — still cheap and useful).
2. **`tests/shim_ld_preload.rs`**: rewrote the test's assertions around
   the actual invariant instead of "the shim logged decision N": expects
   1 or 2 occurrences of `<path> -> ENTER` (1 = only the create reached
   it; 2 = the re-run reached it too, in which case it must show `SKIP
   (already a subvolume)`), and — regardless of which — asserts exactly
   one `ACCEPT (created subvolume)` ever appears and the path is still a
   real subvolume afterward. Removed the `tree`/`stat` dumps, the
   side-channel diag-file reset/read, the `sleep(1s)` experiment, and
   the `log_file.path()` print — all were temporary investigative
   scaffolding, no longer needed now that the test itself tolerates
   both `mkdir` implementations correctly.
3. **`.github/workflows/ci.yml`**: reverted `cargo test` back to plain
   (no `--test-threads=1`, no `--show-output`) — the actual root cause
   was never about concurrency, so serializing the whole suite bought
   nothing and just cost CI time.
4. **`shim/git_core.rs`**'s `.env_remove("LD_PRELOAD")` fix (found along
   the way) stays — a real, independent correctness fix regardless of
   this investigation's outcome.

Verified locally: `cargo fmt --check`, `cargo clippy --all-targets -- -D
warnings`, and `cargo test` (default parallelism, no special flags) all
pass, including this rewritten test. Awaiting a real CI run to confirm
the ubuntu-26.04 leg is green.

## Out of scope

- Not promoting `ubuntu-26.04` out of `continue-on-error` as part of
  this fix — that's a separate decision or (per `ci.plan.md`) once the
  image reaches GA / proves stable over time.
- Not adding uutils-coreutils-specific handling anywhere else in the
  project — this was the only test that assumed a specific `mkdir`
  call pattern; no production code made that assumption.
