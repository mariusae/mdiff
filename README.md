# mdiff

`mdiff` is a terminal diff viewer that behaves like the diff command for the repository it is running in, then renders the result in a pager-friendly terminal UI.

## Backend Behavior

`mdiff` is a chameleon:

- In a Git repository, it runs `git diff ...`
- In a Mercurial or Sapling-style repository, it runs `hg diff ...`
- Outside a repository, it falls back to `diff ...`

That means repository-specific arguments are passed through directly:

```sh
mdiff --cached
mdiff HEAD~3..HEAD
mdiff -c
```

Backend color is disabled before rendering so `mdiff` controls all styling itself.

## Rendering

`mdiff` has two layouts:

- Wide terminals: if the terminal is wider than `120` columns, diffs render side by side
- Narrow terminals: otherwise diffs render in a single-column inline view

Both layouts share the same visual rules:

- No foreground syntax colors
- Changed text is indicated with a subtle background tint
- Unchanged text on the right-hand side is dimmed
- For partially changed lines, unchanged characters are dimmed and changed characters keep the normal intensity
- File headers are shown as bold file paths
- Elided sections are shown in the gutter as italicized counts ending in `⋮`

The side-by-side layout uses a single shared line-number gutter in the middle and shows only right-hand-side line numbers.

## Tinting

The diff tint is derived from the terminal background when the terminal supports background-color queries through `crossterm`'s color-query branch.

- Diff line tint uses a subtle blend
- Search and file-filter highlights use a stronger tint
- If the terminal background cannot be queried, `mdiff` falls back to no custom background tint

## Pager

When output is longer than the current terminal height, `mdiff` opens an interactive pager in the alternate screen.

Pager controls:

- `q`, `Esc`: quit the pager
- `Up`, `PageUp`: move up by one page
- `Down`, `PageDown`, `Space`: move down by one page
- Mouse wheel: scroll
- `g`, `Home`: jump to the top
- `G`, `End`: jump to the bottom
- Terminal resize: rerender immediately with the new width

If stdout is not a terminal, `mdiff` writes the rendered diff directly to stdout instead of opening the pager.

## Search

Press `/` in the pager to open the search HUD at the bottom of the screen.

- Type a plain substring and press `Enter` to search
- All rendered text is searched
- Matches are highlighted with the stronger search tint
- `n`: jump to the next match
- `N`: jump to the previous match
- `Esc`: leave search mode

Search does not wrap. At the ends, the HUD reports `end of file` or `beginning of file`.

## File Filtering

Press `Ctrl-F` in the pager to open the file-filter HUD.

The HUD shows:

- The files currently in the diff
- A prompt line beginning with `› `
- The file currently at the top of the screen in bold

While the filter HUD is open:

- Typing narrows the file list by substring match
- The diff display rerenders immediately to include only matching files
- `Up` and `Down` move between visible files and jump the diff view to that file
- `Home` and `End` jump to the first or last visible file
- `Backspace` removes filter text
- `Enter` or `Esc` closes the filter HUD

Filtering is currently a case-sensitive substring match on file paths.

## Diagnostics

`mdiff --rage` prints diagnostic information about:

- Repository detection
- Backend command selection
- Terminal size and layout mode
- Terminal color support
- Background-color query status
- Computed tint colors
- Relevant environment variables

This is useful when tinting or background detection does not behave as expected.

## Build

```sh
cargo build
```

The debug binary is written to `target/debug/mdiff`.

## Dependency Note

`mdiff` currently depends on a `crossterm` branch with terminal background color query support:

```toml
crossterm = { git = "https://github.com/nornagon/crossterm", branch = "nornagon/color-query" }
```
