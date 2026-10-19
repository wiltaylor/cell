# What each grant does

Default: the app sees the merged image root, a private `/tmp`, a private
`/run/user/<uid>`, a minimal `/dev`, a fresh `/proc`, a read-only `/sys`, and its home.
Nothing else from the host.

| Grant | Binds and environment |
| --- | --- |
| `display = [:wayland]` | `$XDG_RUNTIME_DIR/$WAYLAND_DISPLAY` into the private runtime dir; sets `WAYLAND_DISPLAY`, `XDG_SESSION_TYPE=wayland` |
| `display = [:x11]` | `/tmp/.X11-unix/X<n>`; a fresh wildcard Xauthority cookie; sets `DISPLAY`, `XAUTHORITY`. Keeps the host IPC namespace for MIT-SHM |
| `:x11_fallback` | X11 only when the host has no Wayland socket |
| `audio = [:pipewire]` | `pipewire-0` socket |
| `audio = [:pulse]` | `pulse/native` socket; sets `PULSE_SERVER` |
| `audio = [:alsa]` | `/dev/snd` |
| `input = [:gamepad]` | `/dev/input`, `/dev/hidraw*`, read-only `/run/udev` |
| `input = [:uinput]` | `/dev/uinput` |
| `network = true` | Host network namespace; `/etc/resolv.conf` and `/etc/hosts` read-only |
| `dbus = :session` | Session bus socket; sets `DBUS_SESSION_BUS_ADDRESS` |
| `fonts = true` | `/usr/share/fonts`, `~/.local/share/fonts`, NixOS fonts under `/run/host/*`; a fontconfig snippet in `/etc/fonts/conf.d` |
| `themes = true` | `/usr/share/icons`, `/usr/share/themes` under `/run/host`; sets `XCURSOR_PATH`, passes `XCURSOR_THEME`, `XCURSOR_SIZE` |
| `gpu.enable = true` | `/dev/dri` (Mesa in the runtime does the rest) |
| `gpu.nvidia` | `/dev/nvidia*`; the NVIDIA userspace at `/run/cell/gpu/lib[32]`; a regenerated `ld.so.cache`; glvnd, Vulkan ICD and EGL platform JSON files; `__EGL_VENDOR_LIBRARY_DIRS`, `GBM_BACKENDS_PATH`, `VDPAU_DRIVER_PATH`. `:fetch` downloads the version that matches `/sys/module/nvidia/version` once into `~/.cache/cell/drivers`; `:host` symlinks host libraries; `:auto` tries fetch then host |
| `shm = true` | Host `/dev/shm` |
| `home = :private` | `~/.local/share/cell/apps/<id>/home` bound at the real `$HOME` path |
| `home = :portable` | `<name>.cell.d/state/home` beside the file |
| `home = :host` | The real home, read-write |
| `bind "ro|rw|dev" { host, guest }` | Any extra path; `$PWD` expands to the user's current directory |

Writes outside home go to an overlay upper directory in the state directory. A second
instance of a running app gets a throwaway upper and shares the home.
