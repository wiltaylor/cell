# Help

<purpose>
List the subcommands and show which build tool this session will use.
</purpose>

<workflow>
<step order="1">
Resolve `CELL_TOOL` as described in the `<variables>` section of SKILL.md. Run each
check exactly:
```
[ -n "$CELL_BUILDER" ] && [ -f "$CELL_BUILDER" ] && echo "$CELL_BUILDER"
command -v cell
ls target/release/cell examples/dist/cell-builder.cell 2>/dev/null
```
</step>
<step order="2">
Print the table below with the resolved tool filled in. If no tool was found, print
"no cell tool found" and the first paragraph of `${CLAUDE_SKILL_DIR}/reference/install.md`.
</step>
</workflow>

<output>
```
Cell tool: ./examples/dist/cell-builder.cell

/cell new <app-id> [native-game|wine-game|cli-tool]   write a cell.wcl
/cell build [cell.wcl] [-o out.cell]                  validate, then build
/cell run <file.cell> [info|shell|verify|<command>]   run or inspect
/cell debug <file.cell>                               find why it fails
```
</output>
