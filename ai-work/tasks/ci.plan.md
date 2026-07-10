# Plan: GitHub Actions CI, with real BTRFS tests on the runner

Reference: `/root/code-repos/subvolumize-home/.github/workflows/ci.yml` and its
`tasks/real-btrfs-ci-tests.plan.md` — a sibling project with an already-proven
loop-mounted-real-btrfs CI job. Borrowing its structure and its already-settled
`ubuntu-24.04`/`ubuntu-26.04` GA-vs-preview reasoning (reconfirmed against
`actions/runner-images` today — still accurate: 24.04 is GA and is what
`ubuntu-latest` points to, 26.04 is preview).

## Problem

This project's whole test suite (146 tests as of the last commit) was written
to exercise **real** BTRFS calls directly — no mocking layer, by deliberate
design choice throughout this project's development (see progress notes: "test
the real thing, not a mock"). That's why development happened against this
sandbox's real `/root` BTRFS mount, and why `tests/btrfs_loopback.rs` exists as
an opt-in path for environments that can create their own.

Two consequences that shape this plan, both different from the
subvolumize-home reference:

1. **We have no "mocked, always green" tier.** subvolumize-home's `test` job
   runs everywhere with zero root; ours can't — `btrfs_scratch_dir()` (the
   helper nearly every test uses) needs a real BTRFS-backed directory or the
   test hard-fails (a `.unwrap()` on a real ioctl error), not skips. So unlike
   the reference's `test` + `test-real-btrfs` split, **our main `test` job
   *is* the real-btrfs job** — there's only one meaningful tier today. (Adding
   graceful-skip guards to the whole existing suite, so contributors without
   local BTRFS get clean skips instead of failures, is real, worthwhile future
   work — flagged under "Out of scope" below, not bundled into this plan.)
2. **We don't shell out to `btrfs`/`losetup`/`mount` as our primary
   implementation** — the shim and CLI both use hand-declared raw ioctls
   (`BTRFS_IOC_SUBVOL_CREATE`) directly, not the `btrfs` command-line tool.
   `btrfs-progs` is still needed in CI, but only for `mkfs.btrfs` (formatting
   the loopback image) — nothing in the actual code under test shells out to
   it.

## Design

### Four jobs: `lint`, `test` (real BTRFS, matrixed), `snapper-interop`, `platform-gating`

```yaml
name: CI

# GitFlow-shaped trigger scope: main (release) and develop (pre-release),
# plus hotfix/* and feature/* branches trigger CI directly on push. Any other
# branch name doesn't trigger on push at all - it only gets CI once a PR
# targeting one of these four is opened/updated (`pull_request.branches`
# filters on the PR's *target*, not its source, so a PR from an arbitrary
# branch into develop still triggers this).
on:
  push:
    branches:
      - main
      - develop
      - "hotfix/**"
      - "feature/**"
  pull_request:
    branches:
      - main
      - develop
      - "hotfix/**"
      - "feature/**"

jobs:
  lint:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v7
      - uses: dtolnay/rust-toolchain@stable
        with:
          components: rustfmt, clippy
      - uses: Swatinem/rust-cache@v2
      - name: cargo fmt --check
        run: cargo fmt --check
      - name: cargo clippy
        run: cargo clippy --all-targets -- -D warnings

  # This IS the project's real test tier, not a supplement to a mocked one
  # (see "Problem" above) - every test that needs BTRFS (nearly all of them)
  # only passes here because of the loop-mount setup below.
  #
  # Matrixed on OS, not Rust toolchain version: this suite is sensitive to
  # kernel/btrfs-progs behavior (hand-declared raw ioctls, not the `btrfs`
  # CLI, so there's no abstraction layer between us and exact kernel
  # behavior), not language-version compatibility - a single `stable`
  # toolchain covers that axis fine.
  #
  # ubuntu-26.04 is a preview runner image (no Actions SLA yet, pre-GA) -
  # continue-on-error keeps it visibly running on every push/PR without
  # letting a GitHub-side image hiccup block merges before it's earned that
  # trust. Promote it once proven stable (or it reaches GA) by dropping the
  # continue-on-error condition.
  test:
    runs-on: ${{ matrix.os }}
    continue-on-error: ${{ matrix.os == 'ubuntu-26.04' }}
    strategy:
      fail-fast: false
      matrix:
        os: ["ubuntu-24.04", "ubuntu-26.04"]
    steps:
      - uses: actions/checkout@v7
      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2
      - name: Install btrfs-progs
        run: |
          sudo apt-get update
          sudo apt-get install -y btrfs-progs
      - name: Create loop-mounted btrfs filesystem
        run: |
          truncate -s 2G disk.img
          LOOP=$(sudo losetup --find --show disk.img)
          echo "LOOP_DEV=$LOOP" >> "$GITHUB_ENV"
          sudo mkfs.btrfs -f "$LOOP"
          sudo mkdir -p /mnt/ghostvolumes-test
          sudo mount "$LOOP" /mnt/ghostvolumes-test
          # btrfs subvolume create/delete both need only write permission on
          # the parent dir (not root) once the fs is mounted - confirmed
          # already this session, including that plain rmdir/remove_dir_all
          # (not a special ioctl or `user_subvol_rm_allowed`) correctly
          # deletes real subvolumes since kernel 4.18. So: hand the mount to
          # the runner's own unprivileged user and let `cargo test` run
          # unprivileged too, matching real usage - only this setup step
          # needs sudo.
          sudo chown -R "$(id -u):$(id -g)" /mnt/ghostvolumes-test
          echo "GHOSTVOLUMES_TEST_BTRFS_DIR=/mnt/ghostvolumes-test" >> "$GITHUB_ENV"
      - name: cargo test
        run: cargo test
      - name: cargo test --test btrfs_loopback -- --ignored (bonus, self-skipping)
        # This is the self-contained unshare-based path from
        # tests/btrfs_loopback.rs - separate from (and not required for) the
        # sudo-mounted GHOSTVOLUMES_TEST_BTRFS_DIR above. Whether unshare
        # --user --map-root-user --mount succeeds on this runner depends on
        # whether unprivileged user namespaces are restricted here (Ubuntu's
        # AppArmor-based restriction, present on some 24.04+ configurations) -
        # unknown until this job actually runs on GitHub's infrastructure.
        # Either way this step can't meaningfully fail the job: our own
        # skip-detection reports an unsupported environment as a passing
        # test (with an eprintln'd reason), not a failure - so this is purely
        # informational, first real signal on whether that path works in GH
        # Actions specifically.
        run: cargo test --test btrfs_loopback -- --ignored --nocapture
      - name: Unmount and detach loop device
        if: always()
        run: |
          sudo umount /mnt/ghostvolumes-test || true
          sudo losetup -d "$LOOP_DEV" || true

  # Confirms §8.3's platform gating for real (native build+run), complementing
  # the wasm32-unknown-unknown cross-compile check already done manually this
  # session - that confirmed the *code* compiles correctly gated out on a
  # non-Linux target, this confirms the *fallback binary* actually runs and
  # exits the way real macOS/Windows users would see.
  platform-gating:
    runs-on: ${{ matrix.os }}
    strategy:
      fail-fast: false
      matrix:
        os: [macos-latest, windows-latest]
    steps:
      - uses: actions/checkout@v7
      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2
      - name: Build
        run: cargo build
      - name: Run and confirm the non-Linux fallback message + exit code
        shell: bash
        run: |
          set +e
          output=$(./target/debug/ghostvolumes 2>&1)
          status=$?
          set -e
          echo "$output"
          [ "$status" -ne 0 ] || { echo "expected a nonzero exit code"; exit 1; }
          echo "$output" | grep -q "only supports Linux with BTRFS" || { echo "expected message not found"; exit 1; }

  # Real interop check: a genuine snapper-managed BTRFS layout (not our own
  # test fixtures simulating the .snapshots convention), verifying `scan`
  # against the actual tool it's meant to detect. See "Snapper interop" below
  # for why Timeshift/btrbk aren't included here yet.
  snapper-interop:
    runs-on: ubuntu-24.04
    # Unlike ubuntu-26.04's continue-on-error above (an external factor - a
    # preview image with no SLA), this one is here because the exact snapper
    # CLI behavior (create-config's flags, whether --no-dbus is required in
    # a non-interactive runner, permissions on the resulting .snapshots)
    # is researched but not yet run for real anywhere. Drop this once a real
    # CI run confirms the job as written actually passes.
    continue-on-error: true
    steps:
      - uses: actions/checkout@v7
      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2
      - name: Install btrfs-progs and snapper
        run: |
          sudo apt-get update
          sudo apt-get install -y btrfs-progs snapper
      - name: Create loop-mounted btrfs filesystem
        run: |
          truncate -s 2G disk.img
          LOOP=$(sudo losetup --find --show disk.img)
          echo "LOOP_DEV=$LOOP" >> "$GITHUB_ENV"
          sudo mkfs.btrfs -f "$LOOP"
          sudo mkdir -p /mnt/ghostvolumes-snapper-test
          sudo mount "$LOOP" /mnt/ghostvolumes-snapper-test
      - name: Configure a real snapper config on the mount
        run: |
          sudo snapper --no-dbus -c ghostvolumes-test create-config \
            -f btrfs /mnt/ghostvolumes-snapper-test
          # Confirm snapper actually did what §3 relies on, with a clear,
          # attributable failure here rather than a confusing one two steps
          # later in `ghostvolumes scan` if some snapper version/flag
          # combination behaves differently than expected.
          test -d /mnt/ghostvolumes-snapper-test/.snapshots
          [ "$(stat -c '%i' /mnt/ghostvolumes-snapper-test/.snapshots)" = "256" ]
      - name: Build
        run: cargo build
      - name: ghostvolumes scan detects the real snapper-managed mountpoint
        run: |
          ./target/debug/ghostvolumes scan | tee scan-output.txt
          grep -qx "/mnt/ghostvolumes-snapper-test" scan-output.txt
      - name: ghostvolumes scan --save + reload round-trip
        run: |
          export XDG_CONFIG_HOME=$RUNNER_TEMP/config
          export XDG_DATA_HOME=$RUNNER_TEMP/data
          ./target/debug/ghostvolumes scan --save
          grep -q "/mnt/ghostvolumes-snapper-test" \
            "$XDG_CONFIG_HOME/ghostvolumes/roots.d/00-auto.toml"
          test -s "$XDG_DATA_HOME/ghostvolumes/compiled.tsv"
      - name: Unmount and detach loop device
        if: always()
        run: |
          sudo umount /mnt/ghostvolumes-snapper-test || true
          sudo losetup -d "$LOOP_DEV" || true
```

### Why no separate "mocked test" job (unlike the reference)

Already covered under "Problem" — there's no mocked tier to run separately.
`lint` is the only job that doesn't need real BTRFS.

### `Swatinem/rust-cache@v2`, not in the reference (Python doesn't need it)

Rust compilation is slow enough that CI caching is standard practice, unlike
pip installs. Adding this cache action to every job that builds/tests.

### Disk image size: 2G, matching the reference (not the ~150M this project's own manual testing used)

btrfs's minimum filesystem size is ~109MiB (hit this directly during
`tests/btrfs_loopback.rs` development — the first attempt at 64M failed, fixed
to 150M). But the *full* suite creates many subvolumes across ~150 tests
running against the same mount within one job — 2G gives a comfortable margin
against BTRFS metadata overhead per subvolume, at negligible cost on a CI
runner's disk.

### `user_subvol_rm_allowed` mount option: not needed (unlike the reference)

Checked: nothing in this codebase calls `BTRFS_IOC_SNAP_DESTROY` or shells out
to `btrfs subvolume delete` — all subvolume cleanup (in `convert.rs` and every
test's `TempDir`/`tempfile` teardown) goes through plain `remove_dir_all`
(`rmdir`/`unlink`), which has worked on real BTRFS subvolumes since kernel
4.18 and has already been relied on and confirmed working throughout this
project's whole test suite. subvolumize-home needs the mount option because it
uses the older ioctl-based delete path for its rollback logic; we don't have
an equivalent path.

### Nested "home" subvolume: not needed (unlike the reference)

subvolumize-home creates a nested subvolume inside the mount to mimic "a real
`$HOME` is itself normally a subvolume," because their tool's logic branches
on that. Nothing in ghostvolumes cares whether `GHOSTVOLUMES_TEST_BTRFS_DIR`
itself is a subvolume or a plain directory on the mounted filesystem — our
`btrfs_scratch_dir()` helper just creates tempdirs (plain directories) under
it, and subvolumes get created as *their* children. Simpler: mount the image
directly, `chown` it, done.

### Snapper interop: real, testable today; Timeshift/btrbk: not yet — checked against what `scan` actually implements

`scan`'s implementation (per progress notes) only ever did §3 point 1 — the
unprivileged pass that finds a `.snapshots` sibling subvolume. §3 point 2 (the
*privileged* pass: `snapper list-configs`, parsing
`/etc/timeshift/timeshift.json`, parsing `/etc/btrbk/btrbk.conf`) was
explicitly deferred and **was never implemented**. So:

- **Snapper**: genuinely testable, and worth testing — `.snapshots` is exactly
  the structural fingerprint `scan` looks for, and a real `snapper
  create-config` (verified above: creates `.snapshots` as a real subvolume)
  exercises the real detection path end-to-end, not our own hand-authored
  fixtures.
- **Timeshift/btrbk**: installing and configuring the real tools in CI is
  certainly *possible* (both are apt-installable on Ubuntu), but it would
  verify nothing about ghostvolumes itself right now — there's no code path
  that reads either tool's config, so a "Timeshift interop" test could only
  ever assert "Timeshift itself works," not "ghostvolumes detects it," since
  the corresponding detection logic doesn't exist yet. Building that test now
  would be hollow. Flagged as an open question below rather than silently
  dropped or silently built anyway.

## Step breakdown

1. `.github/workflows/ci.yml` — `lint` job.
2. `.github/workflows/ci.yml` — `test` job (matrixed, real loop-mounted
   BTRFS), including the bonus `btrfs_loopback -- --ignored` step.
3. `.github/workflows/ci.yml` — `snapper-interop` job (real snapper-managed
   BTRFS layout, `continue-on-error` until a real run confirms it).
4. `.github/workflows/ci.yml` — `platform-gating` job (macOS/Windows).
5. Push a branch and confirm every job actually goes green on GitHub Actions —
   loop/mount setup (and, especially, whether the `unshare` bonus step and the
   `snapper-interop` job's exact command sequence actually succeed rather than
   need adjustment) is exactly the kind of thing that needs a real run to
   confirm, per the reference plan's own closing step.
6. `ai-work/tasks/main-plan.md` — add a short note (mirroring the reference's
   "Testing conventions" addition to `design.md`) describing this project's
   one-tier "tests need real BTRFS, CI provides it via loop-mount" convention,
   plus how a contributor with local root/btrfs-progs can opt in the same way
   (`GHOSTVOLUMES_TEST_BTRFS_DIR=/path/to/real/mount cargo test`).

## Out of scope for this plan (possible future work, not bundled in)

- **Retrofitting the existing suite with graceful skip-if-no-BTRFS guards**
  (the way `tests/btrfs_loopback.rs` already does), so a contributor without
  local root/BTRFS gets clean skips instead of hard failures when just running
  `cargo test`. Real, worthwhile, but a separate, more invasive change
  touching most existing test files — not needed to get CI green, since CI
  always provides real BTRFS via the loop-mount step.
- **`publish-dry-run` job** — holding off per review decision below; not
  included in this pass at all (not even as a dry run), until closer to an
  actual crates.io publish.
- **`cargo publish` on tag push** (a real `release` job). Needs a
  `CARGO_REGISTRY_TOKEN` repository secret someone has to create and
  configure — an operational/trust decision, not a code one. Comes later,
  alongside `publish-dry-run` above, once there's a crates.io account and
  the user wants automatic publishing.
- Re-verifying anything already covered by the existing suite structurally —
  nothing here duplicates test logic, it's purely about *where* the existing
  suite runs.

## Decisions already settled (this review round)

- Matrix for `test` stays exactly as originally proposed: `ubuntu-24.04`
  (required) + `ubuntu-26.04` (preview, non-blocking).
- `Swatinem/rust-cache@v2` included from the start, in every job.
- Trigger scope is GitFlow-shaped, not "every branch" like the reference:
  `push` + `pull_request` on `main`, `develop`, `hotfix/**`, `feature/**`
  only — `main` is the future release branch, `develop` is pre-release. A
  branch matching none of these patterns never triggers CI on push; it only
  gets a run once a PR targeting one of the four is opened/updated.
- No `publish-dry-run` (nor a real `release`/`cargo publish` job) in this
  pass at all — holding off until closer to an actual crates.io publish, per
  explicit review decision (a scope narrowing from the original draft, which
  had proposed including the dry-run now since it was cheap).
- Added `snapper-interop`, testing real Snapper/`scan` interop specifically
  (see "Snapper interop" design section) — Timeshift/btrbk interop is a new
  open question below, not bundled in, since `scan` doesn't implement their
  detection yet.

## Open question for review

**Timeshift/btrbk interop** — `scan` only implements the unprivileged Snapper
pass (§3 point 1); §3 point 2 (privileged: `snapper list-configs`,
`/etc/timeshift/timeshift.json`, `/etc/btrbk/btrbk.conf` parsing) was
explicitly deferred and was never built. Testing Timeshift/btrbk interop in
CI now would only prove those tools themselves work, not anything about
ghostvolumes. Options:
1. Ship this plan with Snapper-only interop testing (matches what's actually
   implemented); revisit Timeshift/btrbk once §3 point 2 exists.
2. Implement §3 point 2 (privileged detection) first, as its own separate
   piece of work, *then* extend this CI plan to cover all three tools
   together.

Recommend (1) — keeps this plan scoped to "set up CI for what exists," and
§3 point 2 is a real, separate, non-trivial feature (parsing two more config
formats, plus the "only run under sudo" privileged-detection branch `scan`
doesn't have today at all) that deserves its own plan rather than riding in
as a side effect of a CI task.
