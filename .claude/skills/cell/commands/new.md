# New

<purpose>
Write a `cell.wcl` for an application from one of three recipes, then validate it.
</purpose>

<workflow>
<step order="1">
Parse `$ARGUMENTS`: the first word after `new` is the app id in reverse-DNS form
(`dev.example.mygame`); the second is the recipe, one of `native-game`, `wine-game`,
`cli-tool`. If the recipe is missing, pick `native-game` for a Linux binary or
package, `wine-game` for a Windows executable, `cli-tool` for a command-line program.
If the id is missing, ask for it.
</step>
<step order="2">
Read `${CLAUDE_SKILL_DIR}/reference/recipes.md` and copy the matching recipe into
`cell.wcl` in the current directory. Do not write over an existing `cell.wcl`; ask
first.
</step>
<step order="3">
Fill the recipe in. Replace the id, `name`, `version`, the package list, the `copy`
sources and the `entry.exec`. Keep the comments that explain each grant. Every path in
`copy.from`, `layer.source` and `desktop.icon` is relative to the build file and must
exist; create the directories or ask for the files.
</step>
<step order="4">
Validate:
```
CELL_TOOL validate cell.wcl
```
If it prints `parse error` or `schema errors`, read the message, fix the file with
`${CLAUDE_SKILL_DIR}/reference/wcl-surface.md` open, and run it again. Common causes:
a symbol spelled as a string (`"wayland"` instead of `:wayland`), the reserved word
`none`, a block without braces (`run "cmd" { }` needs the braces), a missing
`import <cell.wcl>`.
</step>
<step order="5">
Report the file and the grants it asks for, one line per grant, and say how to build it
(`/cell build`).
</step>
</workflow>

<success-criteria>
- `CELL_TOOL validate cell.wcl` prints `ok: <name> <version> (<id>)`.
</success-criteria>
