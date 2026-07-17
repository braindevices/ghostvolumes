# `ghostvolumes intercept` — guide

Runs a command with the shim active for that command only, via
`LD_PRELOAD` scoped to the child process. It intercepts `mkdir`/
`mkdirat` and converts a directory into a subvolume — but only if a
`+` decision is already recorded for it. It never prompts.

```bash
ghostvolumes intercept -- <cmd>
```

## Example

```
$ cat .ghostvolumes-decisions
+ node_modules
$ rm -rf node_modules && ghostvolumes intercept -- npm install
# node_modules is now a real BTRFS subvolume - no prompt, no output

$ mkdir build && ghostvolumes intercept -- true
$ cat .ghostvolumes-decisions
+ node_modules
? build
# undecided: left as a plain directory, a "?" marker appended for later review
```

## Notes

- Runs inside arbitrary subprocess trees with no guaranteed terminal, so it can never prompt — see [decide.md](decide.md) or [convert.md](convert.md) to actually resolve a `?` marker.
- Always logs critical events (a subvolume created, an undecided candidate skipped, an unexpected error) to `~/.local/share/ghostvolumes/shim.log` — never to stdout/stderr, since it runs inside a host process. See the README's [Debugging](../README.md#debugging) section for verbosity levels.
- Don't `eval` its `LD_PRELOAD` value into your shell rc file — see the [FAQ](FAQ.md#why-not-just-export-ld_preload-globally) for why.
