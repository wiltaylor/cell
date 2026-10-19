# The cell.wcl surface

A build file starts with `import <cell.wcl>` and declares exactly one `app "<id>" { }`.
Symbols are written `:name`. Lists are `["a", "b"]` or `[:a, :b]`. Blocks with a label
need braces even when empty: `run "cmd" { }`.

| Block / field | Type | Default | Meaning |
| --- | --- | --- | --- |
| `app "<id>"` | label | required | Reverse-DNS id; names the state directory |
| `app.name`, `app.version` | utf8 | id, `0.0.0` | Shown by `info` and `inspect` |
| `app.arch` | `"x86_64"` or `"aarch64"` | `x86_64` | Payload architecture |
| `app.template` | utf8 | — | Name of a `template "<name>" { }` block in the same file whose blocks this app starts from |
| `runtime.base` | utf8 | required | `oci://image:tag` (native registry pull), `docker://image` (local Docker), `tar://path-or-url`, `dir://path` |
| `runtime.multilib` | bool | false | Enable 32-bit packages (Arch multilib). Needed for Wine |
| `runtime.placement` | `:embedded`, `:sidecar`, `:referenced` | `:embedded` | Where the runtime layer goes. `:referenced` writes `runtime-<hash>.layer` beside the output, shared by cells in that directory |
| `runtime.packages` | list<utf8> | — | Installed with the base's package manager (pacman, apk, apt-get, dnf) |
| `runtime.setup` | list<utf8> | — | Shell commands run as root before the packages install |
| `build.copy { from, to }` | block | — | Copy a host path (relative to the build file) into the image |
| `build.run "<cmd>" { }` | block | — | Shell command run as root inside the image, with network |
| `layer "<name>" { source, mount, compression, placement }` | block | — | Extra data layer; `compression` is `:zstd` or `:raw`; `placement` as above |
| `entry.exec` | list<utf8> | required | Command and arguments |
| `entry.cwd` | utf8 | `/` | Working directory; `$PWD` means the directory the user ran the cell from |
| `entry.gamescope` | bool | false | Wrap the command in gamescope (must be in the runtime) |
| `entry.env "<NAME>" { value }` | block | — | Environment variable |
| `entry.command "<name>" { exec }` | block | — | Alternative entry point, run with `CELL_COMMAND=<name>` |
| `isolation.network` | bool | false | Share the host network. `false` gives a private namespace with loopback |
| `isolation.home` | `:private`, `:portable`, `:host` | `:private` | Private home in `~/.local/share/cell/apps/<id>/home`, beside the file in `<name>.cell.d/`, or the real home |
| `isolation.pid` | `:private`, `:host` | `:private` | Pid namespace |
| `isolation.shm` | bool | false | Share `/dev/shm` |
| `isolation.bind "<ro|rw|dev>" { host, guest, optional }` | block | — | Extra bind; `$PWD` expands in both paths; `optional` defaults to true |
| `integration.display` | list of `:wayland`, `:x11`, `:x11_fallback` | — | Display sockets. `:x11_fallback` needs `:wayland` in the list |
| `integration.audio` | list of `:pipewire`, `:pulse`, `:alsa` | — | Audio sockets or `/dev/snd` |
| `integration.input` | list of `:gamepad`, `:uinput` | — | `/dev/input` with udev data; `/dev/uinput` |
| `integration.dbus` | `:off`, `:session` | `:off` | Session bus, unfiltered |
| `integration.fonts`, `integration.themes` | bool | false | Host fonts, icons and cursors under `/run/host` |
| `integration.gpu { enable, nvidia, vaapi }` | block | enable true, nvidia `:auto` | `/dev/dri` plus NVIDIA strategy `:auto`, `:fetch`, `:host`, `:off` |
| `integration.desktop { icon, categories, mime }` | block | — | Desktop entry data stored in the manifest |

Reserved word: `none` cannot be a symbol. The schema uses `:raw` and `:off` instead.
