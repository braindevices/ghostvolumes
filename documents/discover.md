# `ghostvolumes discover` — guide

`discover` is a read-only survey of an arbitrary starting path (`$HOME`
by default) — unlike `convert`/`decide`, it needs no project
registration and never writes anything itself. Its whole job is to
point you at the right `decide`/`convert` command to run yourself.

```bash
ghostvolumes discover [path] [--max-depth N] [--root-is-project]
                       [--no-project <path>]... [--ignore <path>]...
```
## Example

```
ghostvolumes discover ~/ --max-depth 9 --no-project ~/test --ignore ~/.vscode-server --ignore ~/.cargo --ignore ~/go --ignore ~/.local --ignore ~/code-repos
/root/
  watched names present but not yet converted (informational only): .cache

/root/test/aa/pp-with-subvol
  already a subvolume, needs a decision:
    ghostvolumes decide /root/test/aa/pp-with-subvol --add /bb/cc/build --add build
  already a subvolume, but not a watched name - needs clarification:
    ghostvolumes decide /root/test/aa/pp-with-subvol --add /bb/cc/venv3   # or --deny /bb/cc/venv3
  watched names present but not yet converted (informational only): /bb/ca/build

/root/test/project-tracked
  approved ('+') but not yet converted - run to materialize: ghostvolumes convert /root/test/project-tracked   # .cache

/root/test2/project-outroot
  watched names present but not yet converted (informational only): build

```

## Nested suggestions get merged

Two suggested groups in the same lineage would, if both were
registered as separate projects, violate ["projects can't
nest"](project-roots.md). So it merges them.

### effect of `--no-project`
```
ghostvolumes discover ~/test --max-depth 9 --no-project ~/test/aa/pp-with-subvol
/root/test/aa/pp-with-subvol
  already a subvolume, needs a decision:
    ghostvolumes decide /root/test/aa/pp-with-subvol --add build

/root/test/aa/pp-with-subvol/bb/ca
  watched names present but not yet converted (informational only): build

/root/test/aa/pp-with-subvol/bb/cc
  already a subvolume, but not a watched name - needs clarification:
    ghostvolumes decide /root/test/aa/pp-with-subvol/bb/cc --add venv3   # or --deny venv3
```

```
ghostvolumes discover ~/test --max-depth 9
/root/test/aa/pp-with-subvol
  already a subvolume, needs a decision:
    ghostvolumes decide /root/test/aa/pp-with-subvol --add build
  already a subvolume, but not a watched name - needs clarification:
    ghostvolumes decide /root/test/aa/pp-with-subvol --add /bb/cc/venv3   # or --deny /bb/cc/venv3
  watched names present but not yet converted (informational only): /bb/ca/build
```

### use `--root-is-project` if input path is project
```
ghostvolumes discover ~/test/aa/pp-with-subvol --max-depth 9
/root/test/aa/pp-with-subvol
  already a subvolume, needs a decision:
    ghostvolumes decide /root/test/aa/pp-with-subvol --add build

/root/test/aa/pp-with-subvol/bb/ca
  watched names present but not yet converted (informational only): build

/root/test/aa/pp-with-subvol/bb/cc
  already a subvolume, but not a watched name - needs clarification:
    ghostvolumes decide /root/test/aa/pp-with-subvol/bb/cc --add venv3   # or --deny venv3
```

```
ghostvolumes discover ~/test/aa/pp-with-subvol --max-depth 9 --root-is-project
/root/test/aa/pp-with-subvol
  already a subvolume, needs a decision:
    ghostvolumes decide /root/test/aa/pp-with-subvol --add build
  already a subvolume, but not a watched name - needs clarification:
    ghostvolumes decide /root/test/aa/pp-with-subvol --add /bb/cc/venv3   # or --deny /bb/cc/venv3
  watched names present but not yet converted (informational only): /bb/ca/build
```


## What gets reported

Nothing already covered by a `+`/`-` anywhere up to `path` is reported
at all — that's the baseline that keeps the output meaningful instead
of permanently re-suggesting things that are already fully decided.

**Three kinds of genuinely undecided match**, in descending order of confidence:

| Kind | Meaning | Suggests |
|---|---|---|
| Approved candidate | A watched name that's already a subvolume | `ghostvolumes decide <dir> --add <name>` |
| Unwatched subvolume | Already a subvolume, unwatched name | Both `--add <name>` and `--deny <name>` — discover can't ask interactively to default to yes the way `convert`/`decide` can |
| Not yet converted | A watched name that's still a plain directory | Nothing — report-only. `-` would misrepresent "nobody decided" as "a human declined" |

**Two drift kinds**, for a recorded decision that disagrees with the filesystem:

| Kind | Meaning | Suggests |
|---|---|---|
| `DRIFT` (denied but exists) | Recorded `-`, but it's a subvolume anyway | The override command to record `+` instead |
| Approved, not converted | Recorded `+`, but still plain | `ghostvolumes convert <path>` to materialize it |

Only on-disk mismatches are covered — a `+`/`?` decision recorded for a
path that doesn't exist on disk at all isn't detected.
