# Changelog

Notable changes to this project, loosely following [Keep a Changelog](https://keepachangelog.com/).

## 0.2.0 — 2026-07-13

Decision-model rewrite: replaces the git-tracked gate and shell `cd`-hook with an explicit, gitignore-style decision-file model. See [design.md](design.md) for the full rationale.

- **Removed** the git-tracked gate (`is_git_tracked`, shelling out to `git ls-files`) and all VCS-based detection.
- **Removed** the proactive `cd`-hook / shell-integration activation path (`ghostvolumes ensure`, `shell-init`'s hook mode). `ghostvolumes intercept -- <cmd>` is now the sole activation path; `shell-init` prints a diagnostic value only.
- **Added** per-directory `.ghostvolumes-decisions` files (`+`/`-` gitignore-style patterns), resolved live by the shim on every intercepted call.
- **Added** `ghostvolumes convert <path>` for explicit, interactive conversion and decision recording.
- **Added** `ghostvolumes register <path>` and the project-roots list, narrowing decision-file lookups to a project boundary.
- **Added** `GHOSTVOLUMES_AUTO_YES` to opt back into fully-automatic conversion (not recommended — see the FAQ).
- **Added** a startup guard: `ghostvolumes` now refuses to run at all if its own shim is already present in `LD_PRELOAD`, instead of silently misbehaving.
- **Changed** the compiled shim's filename to `libghostvolumes_shim.so` (from a generic `preload.so`), for unambiguous identification in `LD_PRELOAD`/`ps`/`/proc/*/maps` output.
