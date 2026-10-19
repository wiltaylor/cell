# Build

<purpose>
Validate a `cell.wcl` and build it into a `.cell` file.
</purpose>

<workflow>
<step order="1">
Find the build file: the path given after `build`, else `cell.wcl` in the current
directory. Find the output: the path after `-o`, else `<last id segment>.cell` beside
the build file. When several cells share a runtime (`placement = :referenced`), build
them all into one directory, for example `dist/`.
</step>
<step order="2">
Validate:
```
CELL_TOOL validate <file>
```
Stop and fix the file if it does not print `ok:`. See `commands/new.md` step 4.
</step>
<step order="3">
Build:
```
CELL_TOOL build <file> -o <out>
```
The first build of a runtime (base image plus packages) downloads the image and runs
the package manager; an Arch runtime with Mesa takes 2 to 10 minutes. The runtime is
cached by content key in `~/.cache/cell/runtimes`, so a rebuild with the same
`runtime` block takes seconds. Only `build` steps and `layer` blocks run every time.
</step>
<step order="4">
If the build fails, match the last `cell:` line against
`${CLAUDE_SKILL_DIR}/reference/troubleshooting.md`. The most common:
- `target not found: <pkg>`: the package name is wrong for the base distribution.
  Check with `pacman -Si <pkg>` on an Arch host or search the distribution's package
  index. For `lib32-*` packages, `multilib = true` must be set in `runtime`.
- `step failed with exit status N: <cmd>`: a `run` step failed. Read its output above
  the line. Steps run as root inside the image with network.
- `cannot find cell-launcher`: the installed `cell` tool cannot find the launcher.
  Set `CELL_LAUNCHER=<path>` or use the packaged builder, which carries it.
- `no entry for <user> in /etc/subuid`: only the installed tool needs this. Use the
  packaged builder, or add a range with `usermod --add-subuids 100000-165535
  --add-subgids 100000-165535 <user>`.
</step>
<step order="5">
Check the result:
```
CELL_TOOL inspect <out>
CELL_CMD=verify <out>
```
`inspect` lists layers, entry and grants. `verify` must print `ok` for every layer.
</step>
<step order="6">
Report the output path, the size, the layer list, and whether a `runtime-<hash>.layer`
file was written beside it (a referenced runtime must travel with the cell or live in
`~/.local/share/cell/layers`).
</step>
</workflow>

<success-criteria>
- `cell: done: <out> (<n> MiB)` was printed.
- `CELL_CMD=verify <out>` prints `ok` for every layer.
</success-criteria>
