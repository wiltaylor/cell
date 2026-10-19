# Debug

<purpose>
Find why a `.cell` does not start, shows no window, has no sound, or has no GPU.
</purpose>

<workflow>
<step order="1">
Collect facts without starting the GUI:
```
CELL_CMD=info ./app.cell
CELL_CMD=verify ./app.cell
CELL_CMD=shell ./app.cell -c 'id; ls /dev /run/user/*; env | sort'
```
`info` tells you what the host offers (`host:` lines) and what the manifest granted
(`grant:` lines). A grant that is requested but not usable prints a reason, for example
`display: wayland requested, no host socket`.
</step>
<step order="2">
Match the symptom:
- **Launcher error before the app runs** (`cell: ...` and exit 125): look it up in
  `${CLAUDE_SKILL_DIR}/reference/troubleshooting.md`. Namespace errors mean the host
  blocks unprivileged user namespaces; a referenced layer error names the file to put
  beside the cell.
- **No window**: the manifest needs `display = [:wayland]`, `[:x11]` or both. SDL2,
  Qt and GTK apps work on Wayland; SFML 2, most Wine and many older games need
  `:x11`. Test inside: `CELL_CMD=shell ./app.cell -c 'echo $DISPLAY $WAYLAND_DISPLAY;
  ls /tmp/.X11-unix /run/user/*/wayland-*'`.
- **No GPU or software rendering**: `gpu { enable = true }` must be set and the
  runtime must contain Mesa (`mesa`, `vulkan-icd-loader`, `vulkan-intel`,
  `vulkan-radeon`). Test inside with `glxinfo -B` and `vulkaninfo --summary`
  (packages `mesa-utils`, `vulkan-tools`). On NVIDIA hosts, `info` must show a
  `gpu: nvidia ...` line; otherwise set `nvidia = :host` or `:fetch`. PRIME offload:
  `__NV_PRIME_RENDER_OFFLOAD=1 __GLX_VENDOR_LIBRARY_NAME=nvidia`.
- **No sound**: grant `audio = [:pipewire, :pulse]` and put `libpulse` (and
  `lib32-libpulse` for Wine) in the runtime. Test inside: `pactl info`. On the host,
  `pactl list sink-inputs | grep application.name` must list the app while it plays.
- **Wine app without sound or with a broken prefix**: the prefix is in the state
  directory. Ask, then delete `~/.local/share/cell/apps/<id>/home/.wine` and run again.
- **Fonts missing or ugly**: `fonts = true` binds host fonts; or add `ttf-dejavu`.
- **Controller not seen**: `input = [:gamepad]`; Steam Input needs `:uinput` too.
- **File dialogs or links do nothing**: `dbus = :session` (unfiltered).
</step>
<step order="3">
Confirm the fix: edit `cell.wcl`, run `/cell build`, then the failing check again.
Do not change grants the app does not need.
</step>
<step order="4">
Report: symptom, cause with the evidence line, the change made, the check that now
passes.
</step>
</workflow>
