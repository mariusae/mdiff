# mdiff

`mdiff` is a terminal diff viewer that adopts the repository it is running in:

- In a Git repository, it behaves like `git diff ...`
- In a Mercurial/Sapling repository, it behaves like `hg diff ...`
- Outside a repository, it falls back to `diff ...`

When stdout is a terminal wider than `120` columns, `mdiff` renders unified diffs side by side. Otherwise it prints the backend diff output inline.

## Build

```sh
cargo build
```

The binary is written to `target/debug/mdiff`.

## Notes

- Backend color is disabled before rendering so `mdiff` controls the output styling.
- Changed lines on the right pane use a background tint derived from the terminal background when that background can be queried.
- Unchanged text on the right pane is rendered with ANSI dim intensity.
