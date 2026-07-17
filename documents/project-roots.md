# Project roots — guide

**Projects can't nest.** At most one registered project can ever cover
a given path. `convert` registers a project automatically; `ghostvolumes
projects register <path>` sets one up by hand ahead of time.

## Example

```
$ ghostvolumes convert ~/projects/monorepo/packages/foo
Register /home/user1/projects/monorepo/packages/foo as a project? [Y/n] y

$ ghostvolumes convert ~/projects/monorepo
/home/user1/projects/monorepo would nest over the already-registered
project /home/user1/projects/monorepo/packages/foo.
Unregister it and register the broader path instead? [y/N]
```

```
$ ghostvolumes projects list
/home/user1/projects/monorepo/packages/foo
/home/user1/projects/big-app

$ ghostvolumes projects unregister
/home/user1/projects/big-app no longer exists on disk - remove it? [y/N] y
```

## Notes

- Decision (and ignore) files already self-distribute hence no nesting add additional value.
- Two projects that are path-ancestor/descendant of each other but sit on *different* BTRFS volumes are treated as unrelated, not nested.
- A decision file existing at some ancestor with nothing registered covering it (a parent registration possibly forgotten) also warns and asks before registering the narrower path anyway.
- A missing TTY at any of these checks aborts rather than guessing.
- `$XDG_DATA_HOME/ghostvolumes/project-roots.list` a plain-text file one path per line.
- `project-roots.list` and `compiled.tsv` decide decision merge boundaries. It's persistent user data (unlike the disposable `compiled.tsv`) backing it up but don't hand-edit it directly
- Use `ghostvolumes projects register`/`unregister` so a live edit never races the shim's or CLI's own reads and writes of it.
