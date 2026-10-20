# Run

<purpose>
Run a `.cell`, show what it was granted, or open a shell inside its sandbox.
</purpose>

<workflow>
<step order="1">
Parse `$ARGUMENTS`: the `.cell` path, then an optional mode: `info`, `shell`, `verify`,
or the name of a `command` block from the build file. No mode means run the entry
point. Extra words after the mode are passed to the program.
</step>
<step order="2">
Run the matching command exactly:
```
./app.cell [args]                       # entry point
CELL_CMD=info ./app.cell                # host probe, grants, layer files, exec
CELL_CMD=shell ./app.cell -c 'cmd'      # one command inside the sandbox
CELL_CMD=verify ./app.cell              # layer hashes
CELL_COMMAND=<name> ./app.cell [args]   # a named alternative command
CELL_CMD=install ./app.cell             # .desktop entry, icon, MIME types, URL schemes
CELL_CMD=uninstall ./app.cell
CELL_CMD=steam-add ./app.cell           # Steam shortcut with the manifest's artwork (quit Steam first)
CELL_CMD=steam-remove ./app.cell
CELL_CMD=steam-list ./app.cell
```
Overrides for one run: `CELL_NVIDIA=host|fetch|off|auto`, `CELL_NO_GAMESCOPE=1`,
`CELL_LAYER_PATH=dir1:dir2` for referenced layers, `CELL_FILE=<path>` to read the
payload from another file.
</step>
<step order="3">
For a GUI program: ask before you start it, because a window opens on the user's
desktop. Start it in the background with its output in a log file, wait for the
window, and stop it when you are done:
```
./app.cell > app.log 2>&1 & lp=$!
sleep 10
kill -TERM $(pgrep -P $lp)      # pid 1 of the sandbox; the launcher then exits
```
Check with `pgrep -a -f app.cell` that nothing is left. Never use `pkill -f` on a
name that other processes share.
</step>
<step order="4">
A second instance of a running app prints `another instance of <id> is running; this
one keeps no changes outside $HOME` and runs with a throwaway overlay. That is normal.
</step>
<step order="5">
Report the exit code, the first error line from the log if any, and for `info` the
grant lines verbatim.
</step>
</workflow>

<output>
`CELL_CMD=info` prints lines such as:
```
grant: display: wayland (wayland-0)
grant: display: x11 (:1)
grant: gpu: /dev/dri (Mesa from the runtime)
grant: gpu: nvidia 615.71.09 userspace fetched from NVIDIA (cache ...)
layer: runtime (Runtime) at / size 268 MiB Referenced { file: "runtime-2d08....layer" } -> /path/runtime-2d08....layer
```
</output>
