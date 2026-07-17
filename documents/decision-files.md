# Decision files — guide

Per-project, committed to the repo they live in — one
`.ghostvolumes-decisions` file per directory, gitignore-style.

## Example

```diff
# .ghostvolumes-decisions at a project root
+ node_modules                 # matches this name at any depth
+ /dist                        # anchored: this exact location only
+ /packages/*/**/node_modules  # anchored prefix, arbitrary depth after it
- vendor                       # never convert, at any depth
? /build/should-review-this    # pending: noted, not yet a decision
# a real comment, for humans only - never touched by any of the above
```

| Pattern | Meaning |
|---|---|
| `name` | Any depth under this file's directory, by final path component |
| `/name` | Anchored: exact location only |
| `/a/b/**/name` | Anchored prefix, arbitrary depth after it |

## Notes

- Resolution walking up from a candidate: the closest enclosing file with a matching pattern wins; within one file, the last matching line wins.
- `#` is the one prefix reserved for humans — nothing in this tool ever writes or rewrites a `#` line.
- A later real decision for the *same* pattern replaces a `?` line in place rather than leaving both around — answering "no"/"yes" for `/build/should-review-this` above turns that exact line into `-`/`+ /build/should-review-this`, not a second line underneath it.
- [`intercept`](intercept.md) never prompts, so an undecided directory gets skipped and a `?` marker appended for later review; [`convert`](convert.md)/[`decide`](decide.md) are where prompting — and turning a `?` into a real decision — happens.
