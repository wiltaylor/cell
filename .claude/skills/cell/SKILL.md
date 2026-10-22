---
name: cell
description: Builds, runs and debugs Cell portable applications (.cell files for Linux, .cell.exe for Windows) from cell.wcl build files, with the packaged cell-builder.cell or the cell tool. Use when the user wants to package an app, game or CLI tool as a .cell, write or fix a cell.wcl, make a Windows cell (os = "windows") or declare its registry keys, run a Windows cell on Linux through a Wine runner, choose grants (display, audio, GPU, gamepads, network, home), share a runtime layer between cells, or work out why a .cell does not start, has no window, no sound or no GPU.
user-invocable: true
argument-hint: <help|new|build|run|debug> [args...]
allowed-tools:
  - Read
  - Write
  - Edit
  - Glob
  - Grep
  - Bash
---

<overview>
Cell is a portable, sandboxed application format for Linux, with a Windows port that
mounts the same layers through WinFsp. One `.cell` file holds a static launcher, a
complete userland with its own glibc, the application, and a manifest of grants; a
`.cell.exe` holds the Windows launcher, the application and the manifest. A `cell.wcl` file describes the build; `cell build` turns it into a
`.cell`. This skill writes build files, builds them, runs the result, and diagnoses
failures. It works with either the installed `cell` tool or the packaged
`cell-builder.cell`, which needs no installation.
</overview>

<variables>
- `${CLAUDE_SKILL_DIR}`: Path to this skill's directory.
- `$ARGUMENTS`: Full argument string from the user.
- `$1`: Subcommand name.
- `CELL_TOOL`: The command that runs the build tool. Resolve it once per session, in
  this order, and use the first that exists:
  1. `$CELL_BUILDER` if that environment variable names a file.
  2. `cell` on `PATH` (`command -v cell`).
  3. `<repo>/target/release/cell` when the Cell repository is the working tree.
  4. `<repo>/examples/dist/cell-builder.cell` (the packaged builder).
  If none exists, say so and point at `reference/install.md`.
</variables>

<sub-commands>
Route based on `$1`. Default to `help` if no subcommand or unrecognized subcommand.

| Command | Description | File |
|---------|-------------|------|
| help | Show available commands and the resolved `CELL_TOOL` | `${CLAUDE_SKILL_DIR}/commands/help.md` |
| new | Write a `cell.wcl` for an app from a recipe | `${CLAUDE_SKILL_DIR}/commands/new.md` |
| build | Validate and build a `cell.wcl` into a `.cell` | `${CLAUDE_SKILL_DIR}/commands/build.md` |
| run | Run a `.cell`, inspect its grants, open a shell inside | `${CLAUDE_SKILL_DIR}/commands/run.md` |
| debug | Diagnose a `.cell` that fails to start or lacks display, sound or GPU | `${CLAUDE_SKILL_DIR}/commands/debug.md` |

Read the command file, then follow its instructions.
</sub-commands>

<reference-files>
- `${CLAUDE_SKILL_DIR}/reference/wcl-surface.md`: every block and field of `cell.wcl`.
- `${CLAUDE_SKILL_DIR}/reference/grants.md`: what each grant binds and sets.
- `${CLAUDE_SKILL_DIR}/reference/recipes.md`: complete build files for a native GL
  game, a Wine game, a CLI tool, and shared runtimes.
- `${CLAUDE_SKILL_DIR}/reference/troubleshooting.md`: error text to cause to fix.
- `${CLAUDE_SKILL_DIR}/reference/install.md`: how to get the tool or the builder cell.
</reference-files>

<boundaries>
<always>
- Read the command file before acting on it.
- Run `CELL_TOOL validate <file>` before `CELL_TOOL build <file>`.
- Start every `cell.wcl` with `import <cell.wcl>` and exactly one `app` block.
- Grant only what the app needs. The default is no network, a private home, no
  display, no audio and no GPU.
- Stop a running cell by killing the launcher's child, not with `pkill -f`: `lp=$(pgrep
  -f '<name>.cell' | head -1); kill -KILL $(pgrep -P $lp)`. Verify with `pgrep`.
- Use ASD-STE100 Simplified Technical English in comments and reports.
</always>

<ask>
- Which base distribution to use when the app's packages are not in Arch.
- Whether a GUI test may open a window on the user's desktop.
- Before deleting an app's state directory under `~/.local/share/cell/apps/`, because
  it holds saves and Wine prefixes.
</ask>

<never>
- Use `none` as a symbol value in `cell.wcl`; it is a reserved word. Use `raw` for
  uncompressed layers and `off` for D-Bus and NVIDIA.
- Give `home = :host` to a game. Use `:private` or `:portable`.
- Run two builds of the same app id at the same time; they share one build directory.
- Edit files under `~/.cache/cell/` by hand.
</never>
</boundaries>
