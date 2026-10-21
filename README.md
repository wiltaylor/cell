# Cell

**One file. Any Linux. Sandboxed. GPU included.**

Cell is a portable application format for Linux, with a Windows port in
progress. A `.cell` file is a static launcher with a complete userland, the
application and a manifest appended to it. Double-click it, or run it from a
shell, on any distribution: Arch, Debian, Ubuntu, Fedora, Alpine, NixOS. The
host's glibc, Mesa and libraries never matter, because the file carries its
own. The application runs in a sandbox and sees nothing on the host that its
manifest did not grant. A `.cell.exe` is the same format headed by a Windows
launcher; see [Windows](#windows).

```sh
./tuxracer.cell                 # a full 3D game, Wayland or X11, GPU accelerated
./wine-doom.cell                # a Windows game through Wine, with sound
./cell-builder.cell build cell.wcl -o my-app.cell   # the build tool is a cell too
```

Cell takes the single-file idea of AppImage, the full-rootfs isolation of
FlatImage, and Flatpak's grant model and NVIDIA driver handling, and puts
them behind one declarative build file written in
[WCL](https://github.com/wiltaylor/wcl).

## Contents

- [Why](#why)
- [How it compares](#how-it-compares)
- [Quick start](#quick-start)
- [The build file](#the-build-file)
- [Grants](#grants)
- [GPU: OpenGL and Vulkan](#gpu-opengl-and-vulkan)
- [Layers and sharing](#layers-and-sharing)
- [Cell Hub and Cell Launcher](#cell-hub-and-cell-launcher)
- [Running a cell](#running-a-cell)
- [Windows](#windows)
- [State, saves and overrides](#state-saves-and-overrides)
- [Building from source](#building-from-source)
- [Examples and test results](#examples-and-test-results)
- [How it works](#how-it-works)
- [Host requirements](#host-requirements)
- [For AI agents](#for-ai-agents)
- [Status and roadmap](#status-and-roadmap)
- [License](#license)

## Why

Shipping a Linux application is hard because every host is different.
AppImage solves the file problem but not the library problem: the app still
links against the host glibc, so it must be built on an old distribution and
it does not run on NixOS. Flatpak solves the library problem with runtimes
and a sandbox, but needs Flatpak installed and a repository. FlatImage packs
a whole distribution into one file and sandboxes it, but configures the
sandbox by patching the binary and leaves GPU drivers to the author.

Cell's goals:

1. **One file.** A static musl launcher heads the file. It has no dynamic
   dependencies, so it starts on any kernel from 5.11 up.
2. **No host dependency.** The image carries its own glibc and userland.
   The host's libraries are never loaded, with one deliberate exception:
   the NVIDIA proprietary userspace, which must match the host's kernel
   module.
3. **Isolation by default.** No network, no display, no audio, no GPU, no
   host files until the manifest grants them.
4. **Real desktop integration.** Wayland, X11, PipeWire, PulseAudio,
   gamepads, D-Bus, host fonts, OpenGL and Vulkan on Mesa and NVIDIA.
5. **Large payloads.** Layers are squashfs images mounted lazily; a 40 GB
   game starts as fast as a 4 MB tool. Runtimes can be shared between cells.
6. **Declarative builds.** A `cell.wcl` file says what goes in and what the
   app may touch. `cell build` does the rest, without root and without
   Docker.

## How it compares

| | AppImage | Flatpak | FlatImage | **Cell** |
| --- | --- | --- | --- | --- |
| Single file | yes | no | yes | yes (or shared runtime files) |
| Needs anything installed on the host | FUSE | Flatpak | FUSE | FUSE module, user namespaces |
| Own glibc and userland | no | yes | yes | yes |
| Runs on NixOS unchanged | rarely | yes | yes | yes |
| Sandbox | none | bwrap | bwrap | built into the launcher |
| Grants | – | `finish-args` | `fim-perms`, patched into the binary | manifest, immutable |
| Mesa | host | runtime | author installs | runtime |
| NVIDIA | host | per-version extension download | host symlinks | download to match the kernel module, or host symlinks |
| Build definition | scripts | JSON/YAML manifest | shell commands | `cell.wcl` |
| Build needs root or Docker | no | no | no | no |
| Windows | no | no | no | yes, through WinFsp (no sandbox yet) |

## Quick start

You need a Linux host with unprivileged user namespaces and the `fuse`
module (every mainstream distribution has both). Download `cell-builder.cell`
from the [releases page](https://github.com/wiltaylor/cell/releases),
`chmod +x` it, or build it as shown under
[Building from source](#building-from-source).

1. Write `cell.wcl` in a directory with your app's files:

```wcl
import <cell.wcl>

app "dev.example.hello" {
  name    = "Hello"
  version = "1.0"

  runtime {
    base     = "oci://alpine:3.22"
    packages = ["sdl2"]
  }

  build {
    copy { from = "./hello" to = "/app/hello" }
  }

  entry { exec = ["/app/hello"] }

  isolation { network = false home = :private }

  integration {
    display = [:wayland, :x11_fallback]
    audio   = [:pipewire, :pulse]
    gpu { enable = true }
  }
}
```

2. Build it:

```sh
./cell-builder.cell validate cell.wcl
./cell-builder.cell build cell.wcl -o hello.cell
```

The first build pulls the base image from the registry and installs the
packages (seconds for Alpine, minutes for an Arch runtime with Mesa). The
runtime is cached under `~/.cache/cell/runtimes`, so the next build with
the same `runtime` block takes seconds.

3. Run it anywhere:

```sh
./hello.cell
CELL_CMD=info ./hello.cell     # what the host offers and what was granted
```

## The build file

A build file starts with `import <cell.wcl>` and holds one `app` block. The
schema lives in [`schema/cell.wcl`](schema/cell.wcl); WCL rejects unknown
blocks and fields, and `cell validate` checks the rest.

```wcl
import <cell.wcl>

app "dev.wiltaylor.mygame" {
  name    = "My Game"
  version = "1.2.0"
  arch    = "x86_64"

  runtime {
    base      = "oci://archlinux:base"   // oci://, docker://, tar://, dir://
    multilib  = true                     // 32-bit libraries (Wine)
    placement = :referenced              // :embedded, :sidecar or :referenced
    packages  = ["wine", "mesa", "lib32-mesa", "vulkan-icd-loader", "lib32-vulkan-icd-loader"]
    setup     = ["echo runs as root before the packages install"]
  }

  build {
    copy { from = "./dist/game" to = "/app/game" }
    run  "winetricks --unattended corefonts" { }
  }

  layer "assets" {
    source      = "./dist/assets"
    mount       = "/app/game/assets"
    compression = :raw                   // already compressed data: no zstd
    placement   = :sidecar               // a file in mygame.cell.d/
  }

  entry {
    exec      = ["wine", "/app/game/Game.exe"]
    cwd       = "/app/game"
    gamescope = false
    env "WINEDLLOVERRIDES" { value = "dxgi=n" }
    command "editor" { exec = ["wine", "/app/game/Editor.exe"] }
  }

  isolation {
    network = false
    home    = :portable                  // :private, :portable or :host
    pid     = :private
    shm     = false
    bind "ro" { host = "/mnt/mods" guest = "/app/mods" }
  }

  integration {
    display = [:x11, :wayland]
    audio   = [:pipewire, :pulse]
    input   = [:gamepad, :uinput]
    dbus    = :off
    fonts   = true
    themes  = false
    gpu { enable = true nvidia = :auto vaapi = true }
    desktop { icon = "./dist/icon.png" categories = ["Game"] }
  }
}
```

| Block | What it declares |
| --- | --- |
| `runtime` | The base system and the packages installed into it. The result is the runtime layer, cached by content. |
| `build` | `copy` and `run` steps applied on top of the runtime. `run` steps execute as root inside the image with network. Only their changes go into the app layer. |
| `layer` | Extra data mounted at a path inside the sandbox. `:raw` skips compression; `:sidecar` keeps it out of the main file. |
| `entry` | The command, working directory, environment and named alternative commands. `$PWD` in `cwd` means the directory the cell was started from. |
| `isolation` | Namespaces, home mode, and extra binds. `$PWD` works in bind paths too. |
| `integration` | The grants: display, audio, input, D-Bus, fonts, themes, GPU, desktop data. |
| `template` | A named block beside `app` whose sub-blocks an app starts from (`template = "name"`). |

Package names follow the base distribution. `oci://archlinux:base` is the
reference runtime because it tracks current Mesa and glibc; `oci://alpine`
makes tiny tool cells; Debian, Ubuntu and Fedora images work with their
package managers. `none` is a reserved word in WCL, so the schema spells
"no compression" as `:raw` and "no D-Bus" or "no NVIDIA" as `:off`.

## Grants

Nothing is visible by default. Each grant maps to concrete bind mounts and
environment variables, in the spirit of Flatpak's `finish-args`.

| Grant | Effect inside the sandbox |
| --- | --- |
| `display = [:wayland]` | The Wayland socket and `WAYLAND_DISPLAY` |
| `display = [:x11]` | The X socket, a fresh wildcard Xauthority cookie, `DISPLAY`. Keeps the host IPC namespace for MIT-SHM, as Flatpak does |
| `:x11_fallback` | X11 only when the host has no Wayland socket |
| `audio = [:pipewire, :pulse, :alsa]` | The PipeWire socket, the PulseAudio socket with `PULSE_SERVER`, or `/dev/snd` |
| `input = [:gamepad, :uinput]` | `/dev/input`, `/dev/hidraw*` and the udev database; `/dev/uinput` for Steam Input |
| `network = true` | The host network namespace plus `resolv.conf` and `hosts`. `false` gives a private namespace with loopback |
| `dbus = :session` | The session bus, unfiltered |
| `fonts = true` | Host fonts under `/run/host/fonts` with a fontconfig snippet; NixOS paths included |
| `themes = true` | Host icon and cursor themes, `XCURSOR_PATH` |
| `gpu { enable = true }` | `/dev/dri` and the `/sys` trees libdrm reads. Mesa inside the runtime does the rest |
| `gpu { nvidia = ... }` | See below |
| `shm = true` | The host `/dev/shm` |
| `home = :private` | A per-app home in `~/.local/share/cell/apps/<id>/home`, bound at the real `$HOME` path |
| `home = :portable` | The same, but in `<name>.cell.d/state/` beside the file, so saves travel with it |
| `home = :host` | The real home. For tools, not games |
| `bind "ro|rw|dev" { host guest }` | Any other path |

Inside the sandbox the app runs as the real user with the real username,
synthesized into `/etc/passwd`. Every image file shows as owned by that
user; permission bits still apply.

## GPU: OpenGL and Vulkan

**Mesa** (AMD, Intel, Nouveau, llvmpipe) needs nothing from the host. The
kernel DRM interface is a stable ABI, so the Mesa inside the runtime drives
any kernel. Put `mesa`, `vulkan-icd-loader` and the ICDs you want
(`vulkan-intel`, `vulkan-radeon`, `vulkan-swrast`) in the runtime and grant
`gpu { enable = true }`. This is the whole story on NixOS, Steam Deck
hardware, laptops with integrated graphics, and AMD desktops.

**NVIDIA** proprietary drivers need a userspace that matches the loaded
kernel module exactly. The launcher reads `/sys/module/nvidia/version` and
applies the strategy from the manifest:

| `nvidia =` | Behaviour |
| --- | --- |
| `:auto` (default) | `:fetch`, then `:host` when the download fails or the host is offline |
| `:fetch` | Download `NVIDIA-Linux-x86_64-<version>.run` from NVIDIA once into `~/.cache/cell/drivers/`, unpack the libraries, create the SONAME links the installer would, and use them. This is the Flatpak model; nothing is redistributed. On the test machine the first run took 17 s |
| `:host` | Symlink the host's NVIDIA libraries into the sandbox. Works with FHS distributions and with NixOS's `/run/opengl-driver` |
| `:off` | Mesa or software only |

In every mode the launcher binds `/dev/nvidia*`, regenerates `ld.so.cache`
inside the sandbox instead of setting `LD_LIBRARY_PATH` (which breaks Steam
and Wine), and writes the glvnd, Vulkan ICD and EGL platform files. PRIME
offload works with the usual `__NV_PRIME_RENDER_OFFLOAD=1
__GLX_VENDOR_LIBRARY_NAME=nvidia`. Override per run with
`CELL_NVIDIA=fetch|host|off`.

## Layers and sharing

A cell is a stack of squashfs layers mounted over FUSE by the launcher's own
FUSE server, merged by an overlay with a writable upper directory on the
host:

```
| launcher ELF | runtime layer | app layer | data layers... | manifest | trailer |
```

Each layer has a placement:

- `:embedded` — inside the file. The default. One file that works offline.
- `:sidecar` — a file in `<name>.cell.d/`. Keeps multi-gigabyte assets out
  of the main file and lets a publisher update the app without reshipping
  them.
- `:referenced` — a content-addressed file, `runtime-<hash>.layer`, beside
  the cell. Every cell in that directory with the same runtime shares one
  file. The launcher also searches `CELL_LAYER_PATH`, `<name>.cell.d/`,
  `~/.local/share/cell/layers` and `~/.cache/cell/layers`, so a user can
  keep one store of runtimes. `examples/dist/` holds a 268 MiB runtime with
  `tuxracer.cell` and `gl-tools.cell` at 3 MiB each on top of it.

Data layers with `compression = :raw` are served without decompression,
which suits game assets that are already compressed.

## Running a cell

```sh
./app.cell [args]                  # run the entry point; args go to the program
CELL_CMD=info ./app.cell           # host probe, grants, layer files, exec line
CELL_CMD=shell ./app.cell -c 'ls'  # run a command inside the sandbox
CELL_CMD=verify ./app.cell         # check every layer's BLAKE3 hash
CELL_CMD=manifest ./app.cell       # dump the manifest
CELL_COMMAND=editor ./app.cell     # run a named alternative command
cell inspect app.cell              # the same from the build tool
```

| Variable | Effect |
| --- | --- |
| `CELL_NVIDIA` | `auto`, `fetch`, `host` or `off` for this run |
| `CELL_NO_GAMESCOPE` | Skip the gamescope wrapper |
| `CELL_LAYER_PATH` | Extra directories for referenced layers, colon separated |
| `CELL_FILE` | Read the payload from another file, for a cell on a `noexec` mount |
| `CELL_STEAM_DIR`, `CELL_STEAM_USER` | Which Steam installation and account `steam-add` edits |

A second instance of a running app starts with a throwaway overlay and
prints a notice; both instances share the home directory. Killing the
launcher kills the whole sandbox: pid 1 inside is a small init that gets
`SIGKILL` when the launcher dies, forwards signals, reaps orphans and waits
for daemons such as `wineserver` to finish after the entry point exits.

## Desktop entries, URL schemes and Steam

A cell can register itself with the desktop and with Steam. The art and the
metadata come from the build file, so a published cell carries everything:

```wcl
integration {
  desktop {
    icon       = "./art/icon.png"
    comment    = "Downhill racing starring Tux"
    categories = ["Game", "SportsGame"]
    mime       = ["application/x-tuxracer-course"]
    protocols  = ["tuxracer"]                   // tuxracer:// links open the cell
    keywords   = ["penguin", "ski"]
  }
  steam {
    portrait = "./art/portrait.png"             // library capsule, 600x900
    wide     = "./art/wide.png"                 // header capsule, 920x430
    hero     = "./art/hero.png"                 // library hero, 1920x620
    logo     = "./art/logo.png"                 // transparent logo over the hero
    tags     = ["Cell"]
  }
}
```

```sh
CELL_CMD=install ./tuxracer.cell        # .desktop entry, icon, MIME types, URL schemes
CELL_CMD=uninstall ./tuxracer.cell
CELL_CMD=steam-add ./tuxracer.cell      # non-Steam game with the artwork above
CELL_CMD=steam-remove ./tuxracer.cell
CELL_CMD=steam-list ./tuxracer.cell     # every shortcut in the Steam library
CELL_CMD=desktop-entry ./tuxracer.cell  # print the .desktop text
```

`install` writes `~/.local/share/applications/<id>.desktop` with `Exec`
pointing at the cell's current path, puts the icon in the hicolor theme,
turns every `command` block into a desktop action, and makes the cell the
default handler for its URL schemes with `xdg-mime`. A link such as
`tuxracer://course/1` then starts the cell with the URL as its argument.

`steam-add` edits `userdata/<user>/config/shortcuts.vdf` (Steam's binary
KeyValues format), keeps a backup beside it, writes the artwork into
`config/grid/` under the app id Steam derives for the shortcut, and prints
the `steam://rungameid/` URL. Steam reads the file at start-up only, so quit
Steam first; on a Steam Deck use desktop mode. The cell then appears in the
library and in gaming mode like any other game. `CELL_STEAM_DIR` and
`CELL_STEAM_USER` pick a Steam installation and account when there are
several.

## Windows

A cell with `os = "windows"` in its build file is built on Linux like any
other and comes out as `<name>.cell.exe`: the same trailer, manifest and
squashfs layers behind a Windows launcher instead of the musl one. Nothing
is extracted at build time and nothing from the host is needed at run time
except [WinFsp](https://winfsp.dev), which supplies the file system driver.

```wcl
import <cell.wcl>

app "dev.example.game" {
  name    = "My Game"
  version = "1.0"
  os      = "windows"

  build {
    copy { from = "./dist/game" to = "/app/game" }
  }

  layer "assets" {
    source      = "./dist/assets"
    mount       = "/app/game/assets"
    compression = :raw
  }

  entry {
    exec = ["/app/game/Game.exe", "-data", "$CELL_ROOT/app/game/assets"]
    cwd  = "/app/game"
  }

  isolation { home = :private }
}
```

How it runs:

- **Tier 0, WinFsp installed.** The launcher loads `winfsp-x64.dll` at run
  time and serves the layers as one union volume mounted at
  `%LOCALAPPDATA%\cell\mnt\<id>`. Reads come straight from the `.cell.exe`
  file, lazily, so a large game starts as fast as a small tool. Writes go
  to `%LOCALAPPDATA%\cell\apps\<id>\upper` with copy-on-write; deleting a
  layer file writes a whiteout, like overlayfs. The volume is
  case-insensitive, executables and DLLs load from it, and child processes
  see it. Dropping the launcher removes the mount.
- **Tier 1, no WinFsp.** The launcher extracts the layers once into
  `%LOCALAPPDATA%\cell\apps\<id>\root`, keyed by the layer hashes, and runs
  the app from there. `CELL_TIER=extract` forces this tier.

The entry point uses the same `/app/...` paths as a Linux cell: `/` is the
root of the layer stack. `$CELL_ROOT` in an argument, `cwd` or an `env`
value expands to the host path of that root, and the app also gets it as
the `CELL_ROOT` variable. Apps must be relocatable, which Windows games and
portable apps already are; a WinFsp volume cannot overlay `C:\`.

`home = :private` and `:portable` set `USERPROFILE`, `APPDATA`,
`LOCALAPPDATA`, `TEMP` and `TMP` to a per-app home, so saves and settings
land there; `:host` leaves them alone. `CELL_CMD=info`, `verify`,
`manifest` and `shell` work as on Linux.

**The sandbox.** The app runs in an AppContainer unless the manifest says
`isolation { sandbox = :off }` or the user sets `CELL_SANDBOX=off`. That
is the same boundary Windows uses for Store apps and for Win32 app
isolation: a low-integrity token with its own SID, which can open nothing
the launcher did not grant. The grants map like this:

| Manifest | In the container |
| --- | --- |
| `network = false` | No network capability: every socket call fails at once |
| `network = true` | The `internetClient`, `internetClientServer` and `privateNetworkClientServer` capabilities |
| `bind "ro" { host }` | A read ACE for the container on the host path; `guest` has no meaning without a mount namespace |
| `bind "rw" { host }` | A full-access ACE on the host path |
| `home = :private` | The per-app home gets an ACE; the real profile stays invisible |

One host step is needed before a sandboxed app can use the mounted tier.
WinFsp creates every volume device with a descriptor that admits Everyone
but no app package, and an AppContainer must pass a second check that
only its own SIDs can satisfy. `CELL_CMD=setup`, run once as
administrator (it asks for elevation), adds app packages to WinFsp's
device class security in the registry; every later mount picks it up
without a reboot. Until then the launcher notices, says so, and runs the
sandboxed app from an extracted copy instead, so the boundary never
silently drops.

**The registry.** There is no virtual hive: the app reads and writes the
user's real `HKEY_CURRENT_USER`, and a cell declares the keys it needs.
The launcher creates them before the app starts and gives the container
access to them; every other key stays read-only or invisible to it.

```wcl
registry {
  import = "./settings.reg"                 # a regedit export, HKCU keys only
  grant  = ["HKCU/Software/Vendor/Shared"]  # reachable, nothing written
  key "HKCU/Software/Vendor/Game" {
    value "@"          { string = "Game" }
    value "InstallDir" { string = "$CELL_ROOT/app/game" }
    value "Fullscreen" { dword  = 0 }
    value "Mods"       { strings = ["base", "extra"] }
  }
}
```

A value is written when it is missing, so the user's later changes
survive; a value that names `$CELL_ROOT` is rewritten at every start,
because the mounted root can move. Types are `string`, `expand`
(`REG_EXPAND_SZ`), `dword`, `qword`, `hex` (binary) and `strings`
(`REG_MULTI_SZ`); `"@"` is a key's default value. A `.reg` import takes
regedit's UTF-16 or a UTF-8 file, every type above, and refuses
deletions and keys outside `HKCU`. `CELL_CMD=registry` prints the
declaration as a `.reg` file, and `CELL_CMD=registry-clean` removes the
declared keys again.

What a Windows cell does not have yet, in order of likely arrival:

- **`run` build steps.** The build tool runs on Linux, so a Windows
  installer cannot execute during a build; `copy` the installed files.
- Desktop and Steam integration (`CELL_CMD=install`, `steam-add`), the PE
  icon, code signing, and 32-bit or ARM64 launchers.

The build tool rejects names Windows cannot hold (`:`, `?`, `CON`, trailing
dots) and files that differ only in case. `examples/win-test/` is a cell
that tests the launcher from inside (file reads, copy-up, whiteouts,
renames, memory mapping, DLL loading, a child process, the private home,
the declared registry keys)
and `examples/wine-doom/cell-windows.wcl` packs Chocolate Doom's own Windows
build. `lab/windows/` is a vmlab lab with a Windows 11 machine for running
them; `just win-run win-test` copies a built example in and runs it.

## Cell Hub and Cell Launcher

The server side and the desktop library live in their own repository,
[wiltaylor/cell-hub](https://github.com/wiltaylor/cell-hub): Cell Hub
hosts cells for download, runs game servers from cells with a console,
config editor and backups, and pairs people by keypair profiles instead of
accounts; Cell Launcher is the Steam-style desktop library that plays
cells and talks to a hub. Both depend on this repository's `cell-core`
crate for the file format. This repository is the format itself: the
launcher, the build tool, the schema and the examples.

## State, saves and overrides

Writable state lives in `~/.local/share/cell/apps/<id>/`:

- `upper/` is the overlay upper directory: everything the app writes outside
  its home, for example a Wine registry under `/etc` or a shader cache
  under `/var`.
- `home/` is bound at the real `$HOME` path when `home = :private`.
- `instance.lock` is the per-app lock.

Delete the directory to reset an app, Wine prefix included. With
`home = :portable` the same layout lives in `<name>.cell.d/state/` beside
the file. Caches for downloads live in `~/.cache/cell/`: `oci/` (image
blobs and extracted bases), `runtimes/` (built runtime layers), `drivers/`
(NVIDIA userspace), `build/` (scratch, safe to delete).

## Building from source

Requirements on the build machine:

- A Rust toolchain with the `x86_64-unknown-linux-musl` target
  (`rustup target add x86_64-unknown-linux-musl`). For Windows cells also
  the `x86_64-pc-windows-gnu` target and `mingw-w64`
  (`x86_64-w64-mingw32-gcc`).
- `zig`, used as the musl C compiler for the zstd and ring sources through
  `tools/musl-cc`.
- `squashfs-tools` (`mksquashfs`) for the build tool.
- [`just`](https://github.com/casey/just) for the recipes below (optional).
- For the installed build tool: `newuidmap` and an `/etc/subuid` entry
  (`usermod --add-subuids 100000-165535 --add-subgids 100000-165535 $USER`).
  The packaged `cell-builder.cell` does not need these.

```sh
just all-build                   # the static launcher and the cell tool
just example-build cell-builder  # the build tool as a cell, in examples/dist/
just example-build tuxracer      # Extreme Tux Racer on an Arch runtime
just workspace-check             # tests and clippy
just launcher-build-windows      # the Windows launcher (cell-launcher.exe)
just win-test-build              # the Windows test program and its DLL
just example-build-windows win-test            # examples/dist/win-test.cell.exe
just example-build-windows wine-doom cell-windows   # examples/dist/wine-doom.cell.exe
```

The workspace has three crates:

| Crate | Role |
| --- | --- |
| `cell-core` | The file format and manifest, a random-access squashfs reader, the FUSE server, the sandbox (namespaces, mounts, grants, `pivot_root`, `exec`), host probing, NVIDIA driver fetching |
| `cell-launcher` | The static musl binary that heads every `.cell`: 3 MiB, no dynamic dependencies. Built for `x86_64-pc-windows-gnu` it is the Windows launcher: WinFsp union mount or extraction, a job object, the private home |
| `cell` | The build tool: reads `cell.wcl` through the WCL crate, pulls OCI images, runs builds in a user namespace, packs and assembles |

The WCL dependency is pinned to a git revision of
[wiltaylor/wcl](https://github.com/wiltaylor/wcl).

## Examples and test results

All examples build into `examples/dist/` with `just example-build <name>`.
The Wine example needs `examples/wine-doom/fetch-assets.sh` first, which
downloads Chocolate Doom and Freedoom.

Tested on 2026-10-19 and 2026-10-20 on CachyOS (kernel 7.2), KDE on
Wayland, Intel Iris Xe plus an NVIDIA RTX 4060 on driver 615.71.09:

| Example | Size | Result |
| --- | --- | --- |
| `alpine-test` | 5 MiB | Entry point runs as the real user; FUSE layers, overlay, private `/dev`, `/tmp`, `/run` |
| `tuxracer` | 3 MiB + 268 MiB shared runtime | OpenGL 4.6 and Vulkan 1.4 on Intel through the runtime's Mesa; NVIDIA through the fetched userspace and through host libraries; a full race renders at 2560x1600; audio reaches the host |
| `gl-tools` | 3 MiB + the same runtime | glxinfo, vulkaninfo, vkcube, eglinfo on the shared runtime file |
| `wine-doom` | 767 MiB | Wine prefix created on first run; Minesweeper and Chocolate Doom (Freedoom) run through Wine with wined3d and DirectSound audio |
| `cell-builder` | 12 MiB | The build tool itself; built `gl-tools` and a fresh Arch runtime with pacman from inside its own sandbox |
| `openttd` | 4 MiB + 258 MiB runtime | OpenTTD client as the entry point and `command "server"` for the dedicated server, which Cell Hub can host |

Windows, tested on 2026-10-21 on a Windows 11 24H2 (26100) vmlab machine with
WinFsp 2.x installed:

| Example | Size | Result |
| --- | --- | --- |
| `win-test` | 12 MiB | All 21 checks pass inside the AppContainer through the WinFsp union volume, again with `CELL_TIER=extract`, and 19 with `CELL_SANDBOX=off`: lower reads, case-insensitive lookup, a 16 MiB pattern read and memory-mapped, copy-up on append, whiteout on delete, mkdir and rename, DLL loading from the layer, a `:raw` data layer at `/assets`, a child `cmd` reading the layer, a writable private home, an AppContainer token, a blocked network, the declared registry values of every type (inline and from a `.reg` import), a declared key that keeps a counter between runs, a grant-only key that is writable, and an undeclared key that the container cannot create. Without the setup step the launcher falls back to the extracted copy and still passes. The mount point is gone after exit |
| `wine-doom` (`cell-windows.wcl`) | 63 MiB | Chocolate Doom's 32-bit Windows build runs Freedoom from the mounted volume, launched from the Run dialog; the mount is also visible to other sessions on the machine |

## How it works

At run time the launcher:

1. Reads the 64-byte trailer at the end of its own executable, then the
   CBOR manifest with the layer table. Resolves referenced layers.
2. Probes the host: kernel, user namespaces, `/dev/fuse`, display sockets,
   audio sockets, GPU vendor, NVIDIA module version.
3. Takes the instance lock and, if needed, fetches the NVIDIA userspace
   while it still has host network.
4. Enters a user namespace in which the real uid is root, plus a mount
   namespace and, unless `network = true`, a network namespace with
   loopback up.
5. Mounts a tmpfs work area and each layer with its built-in FUSE server by
   a direct `mount(2)` on `/dev/fuse`. No `fusermount`, no setuid helper.
6. Mounts the overlay root, a private `/dev`, `/tmp` and `/run`, a read-only
   `/sys`, the device nodes, and every grant.
7. Clones a child straight into a new pid namespace. The child mounts
   `/proc`, pivots into the new root, runs `ldconfig` for the GPU libraries,
   enters an inner user namespace that maps root back to the real uid, and
   executes a small init which starts the entry point.
8. Waits, forwards signals, and returns the exit code. The kernel tears the
   namespaces down with the last process.

The build tool uses the same sandbox code to run package managers and
build steps, as root in a user namespace mapped with the caller's
subordinate ids, or with one mapped id when running inside a cell.
`DESIGN.md` has the full design and the implementation notes.

## Host requirements

Windows: Windows 10 or 11 x64, and [WinFsp](https://winfsp.dev) for the
mounted tier (one installer, no reboot), plus `CELL_CMD=setup` once as
administrator so sandboxed apps can use that tier. Without either the
launcher extracts the layers once.

Linux:

| Needs | Why |
| --- | --- |
| Linux 5.11 or newer | Overlayfs inside a user namespace. FUSE in a user namespace needs 4.18 |
| Unprivileged user namespaces | `kernel.unprivileged_userns_clone=1`, `user.max_user_namespaces>0`. On Ubuntu 23.10 and later also `kernel.apparmor_restrict_unprivileged_userns=0` or an AppArmor profile |
| `/dev/fuse` | The `fuse` module |
| Linux 6.9 or newer | Optional: FUSE passthrough for raw data layers |

The launcher reports a missing requirement with the fix. A host that forbids
user namespaces cannot run cells today; the `proot` fallback from the design
is not implemented.

## For AI agents

`.claude/skills/cell/` is a Claude Code skill: it writes `cell.wcl` files
from recipes (native game, Wine game, CLI tool), builds them with the tool
or the builder cell, runs the result and diagnoses missing windows, sound or
GPU. Copy the directory into another project's `.claude/skills/` and point
`CELL_BUILDER` at `cell-builder.cell`.

## Status and roadmap

Working: the launcher, layers, overlay, init, every grant above, Mesa and
NVIDIA (fetch and host), native OCI pulls with token auth and digest
verification, `docker://`, `tar://` and `dir://` bases, data layers,
sidecar and referenced placement, named commands, instance locking, the
packaged builder, the agent skill.

Working on Windows: the `.cell.exe` format, the WinFsp union mount, the
extract fallback, the AppContainer sandbox with `network` and `bind`
grants, `CELL_CMD=setup`, `$CELL_ROOT`, the private home, the job object,
declared registry keys with `.reg` import and sandbox grants, the Windows
name checks in the build tool.

Planned:

- Windows: desktop and Steam integration, the PE icon, signing
- seccomp filters and a filtered D-Bus proxy for portals
- signing with ed25519 and a verification policy
- fetching a missing referenced layer from a URL recorded in the manifest
- an `aarch64` launcher build
- a `proot` fallback for hosts without user namespaces

## Releasing

CI runs on every push. A push to `main` whose head commit carries a trailer
publishes a release:

```
feat: add the thing

release: true
```

`release: true` makes a stable release; `pre-release: true` makes an
`X.Y.Z-alpha` pre-release. The version comes from the last `v*` tag, bumped
by the conventional-commit subjects since it (`feat:` minor, `fix:` and the
other types patch, `!` or `BREAKING CHANGE` minor while the major is 0). The
workflow builds the Linux launcher, the Windows launcher, the tool and
`cell-builder.cell` on a GitHub runner, smoke-tests the builder cell, tags
the commit, and attaches the artifacts with a `SHA256SUMS` file.

## License

MIT. See [LICENSE](LICENSE).
