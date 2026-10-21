# Cell: a portable, sandboxed application format for Linux

Status: design draft, 2026-10-19.
The name "Cell" and the `.cell` extension are placeholders.

## 1. Goals

1. One file. A `.cell` file is a static ELF launcher with the application
   image appended. It runs on any x86_64 or aarch64 Linux host.
2. No host dependency. The launcher is static (musl). The image carries its
   own glibc and userland. The host glibc version does not matter. NixOS,
   Alpine, Arch and Ubuntu are all valid hosts.
3. Isolation by default. The app cannot read or write host files, sockets or
   devices unless the manifest grants them.
4. Full desktop integration on request: Wayland, X11, OpenGL, Vulkan, audio,
   gamepads, D-Bus.
5. Large payloads. Games with tens of gigabytes of assets must start fast and
   must not be copied or extracted at launch.
6. Declarative build. A WCL file describes the base system, the build steps,
   the entry point, the isolation and the integration. One command builds the
   `.cell` file from it.

## 2. What the existing formats teach us

| Format | Launcher | Userland | Isolation | GPU | Big payloads |
| --- | --- | --- | --- | --- | --- |
| AppImage | Static musl runtime, squashfs over FUSE, extract fallback | Host glibc. The app must be built on an old distro | None | Host drivers, host Mesa. Works because it is the host | squashfs, lazy reads |
| Flatpak | Host-installed `flatpak` and bubblewrap | Runtime layer plus app layer, OSTree | bwrap, default deny, portals, filtered D-Bus | Mesa in the runtime. NVIDIA via a per-kernel-version extension downloaded from NVIDIA | OSTree deltas, not single-file |
| FlatImage | Static binary, copies itself to `/tmp`, DwarFS over FUSE, bundled bwrap | Full Alpine or Arch rootfs with its own glibc | bwrap, user namespaces, default deny, `fim-perms` | `gpu` binds `/dev/dri` and symlinks host NVIDIA libs. Mesa is installed in the image by the author | DwarFS layers, read-write commits into the binary |
| GameImage | FlatImage | FlatImage Arch base plus Wine, RetroArch, PCSX2, RPCS3 | FlatImage | FlatImage | Saves in the image or beside it |

What we take from each:

- From AppImage: the static launcher, lazy FUSE mounting, the extract-and-run
  fallback, and the single-file layout.
- From Flatpak: default-deny permissions, the socket and device grant model,
  the synthesized Xauthority cookie, host fonts under `/run/host`, and the
  NVIDIA "fetch the matching userspace" model.
- From FlatImage: the full rootfs inside the image, user-namespace
  bubblewrap-style setup without root, and the layer stack.
- From GameImage: portable save data beside the file, and the runner
  templates for Wine and emulators.

What we change:

- The launcher does not copy itself to `/tmp`. It mounts from inside a user
  namespace, so it needs no `fusermount` and no setuid helper.
- The image is immutable and signable. Configuration edits do not patch the
  binary. User overrides live in a separate file.
- GPU drivers are a first-class runtime concern, not a package the author
  installs by hand.
- The build is a declarative WCL file, not a sequence of `fim-*` commands.

## 3. Host requirements and fallback ladder

The launcher probes the host and picks the best mode. The user can see the
chosen mode with `CELL_CMD=info`.

| Tier | Needs | Mount | Writes | Isolation |
| --- | --- | --- | --- | --- |
| 0 (target) | Unprivileged user namespaces, `/dev/fuse`, kernel 5.11 or newer | FUSE inside the user namespace | Kernel overlayfs inside the user namespace | Full |
| 1 | User namespaces, `/dev/fuse`, kernel older than 5.11 | FUSE inside the user namespace | Bind-mounted writable directories on fixed paths | Full |
| 2 | User namespaces, no `/dev/fuse` | Layers extracted once to the cache directory | Overlayfs or binds as above | Full |
| 3 | No user namespaces | Extracted layers | Binds | Degraded: `proot`-style ptrace emulation. Slow. Off by default, the user must opt in |

Kernel facts the design rests on:

- FUSE mounts are allowed in an unprivileged user namespace since Linux 4.18.
  The process holds `CAP_SYS_ADMIN` in its own namespace and opens `/dev/fuse`
  itself. No setuid `fusermount` is needed.
- Overlayfs mounts are allowed in a user namespace since Linux 5.11.
- FUSE passthrough (Linux 6.9) lets the FUSE server hand a file to the kernel
  so reads bypass the FUSE daemon. The launcher uses it for uncompressed data
  layers when the kernel offers it. This gives near-native read speed for game
  assets.

Known host problems the launcher must detect and explain:

- Ubuntu 23.10 and later restrict unprivileged user namespaces with AppArmor.
  The namespace is created but it has no capabilities. The launcher detects
  this and prints the fix: an AppArmor profile for the launcher, or the
  `kernel.apparmor_restrict_unprivileged_userns=0` sysctl.
- A `noexec` mount, for example a FAT USB stick. The kernel refuses to run the
  file. The fix is `cell run <file>` from a host-installed copy of the
  launcher, which reads the payload from the given path.
- A missing `fuse` module. Tier 2 applies.

## 4. File layout

```text
+--------------------------------------------------+
| ELF launcher (static musl, ~2-3 MiB)             |
+--------------------------------------------------+
| Layer 0: runtime  (squashfs, zstd)               |
+--------------------------------------------------+
| Layer 1: app      (squashfs, zstd)               |
+--------------------------------------------------+
| Layer 2..n: data  (squashfs, zstd or none)       |
+--------------------------------------------------+
| Manifest (CBOR, compiled from the WCL file)      |
+--------------------------------------------------+
| Signature block (ed25519 over the layer table)   |
+--------------------------------------------------+
| Trailer (fixed 64 bytes): magic, version,        |
|   manifest offset and size, layer table offset,  |
|   layer count, flags                             |
+--------------------------------------------------+
```

- The launcher finds the trailer by reading the last 64 bytes of its own
  executable (`/proc/self/exe`). It never needs its own ELF section layout.
  Appending a layer only rewrites the tail of the file.
- Each layer table entry holds: role, mount path inside the sandbox, offset,
  size, compression, BLAKE3 hash, and placement (`embedded` or `sidecar`).
- A sidecar layer lives in a file beside the main file:
  `MyGame.cell.d/assets.layer`. The launcher resolves sidecars relative to the
  main file. Sidecars keep the main file under the 4 GiB FAT32 limit and let a
  publisher ship a small update without reshipping the assets.
- A layer can be `embedded`, `sidecar` or `referenced`. A referenced layer
  is not in the file. It is a content-addressed file,
  `<layer>-<hash16>.layer`, beside the cell. Every cell in that directory
  with the same layer bytes shares the one file. The launcher also searches
  `CELL_LAYER_PATH`, `<name>.cell.d/`, `$XDG_DATA_HOME/cell/layers` and
  `$XDG_CACHE_HOME/cell/layers`, so a user can keep one store of runtimes.
  Fetching a missing referenced layer from a URL is future work. The
  default is embedded, because one file that works offline is the point.
- The manifest is the compiled form of the WCL file. The launcher parses CBOR,
  not WCL. This keeps the launcher small and keeps the WCL parser in the build
  tool.

Image format choice: squashfs with zstd. Reasons:

- Rust can read it with no C code (`backhand` crate), so the launcher stays
  fully static and single-language.
- It supports a 1 MiB block size, which suits large sequential game assets.
- `compression = :none` on a data layer gives zero-decompression reads and
  enables FUSE passthrough.
- DwarFS gives better ratios through similarity ordering. It is a C++ project
  with heavy dependencies and slow image build times. It can be added later as
  a second layer codec; the layer table already carries a codec field.

## 5. Runtime sequence

1. Read the trailer, the layer table and the manifest. Verify the signature if
   the user policy demands it.
2. Resolve the user override file, if any:
   `$XDG_CONFIG_HOME/cell/overrides/<app-id>.wcl`, compiled on the fly by a
   tiny subset parser, or `CELL_*` environment variables and `--cell-*` flags.
   Overrides can only narrow or widen grants, never change the entry point
   unless `CELL_CMD=shell` is used.
3. Probe the host: kernel version, user namespaces, `/dev/fuse`, overlayfs in
   userns, GPU vendor, display sockets, audio sockets.
4. `unshare` user, mount, pid, ipc, uts and cgroup namespaces. Map the host
   uid and gid to the same ids inside. Unshare the network namespace too when
   `network = false`, then bring up `lo`.
5. Mount a private tmpfs root. Mount each layer with the in-process FUSE
   server. Mount the overlay: lower = layers, upper = the app's state
   directory, work = a sibling. Mount it at the new root.
6. Set up `/dev`, `/proc`, `/sys`, `/etc` fragments, `/run/host`, and the
   grants from the manifest (section 7). Run the GPU setup (section 8).
7. Write `/etc/passwd` and `/etc/group` entries for the current user, and
   `/etc/ld.so.cache` for the merged library paths, into the private tmpfs.
8. `pivot_root`, close file descriptors, apply the seccomp filter, drop the
   capabilities held in the user namespace, and `exec` the entry point.
9. The parent process stays as a supervisor: it forwards signals, reaps
   children, and unmounts on exit. If the supervisor dies, the kernel tears
   down the mount namespace with the last process in it, so no janitor daemon
   is needed.

The launcher never runs `bwrap`, `squashfuse` or `fusermount` as separate
programs. Every step is a syscall from the one static binary. This is the
single biggest difference from FlatImage, which extracts and runs several
bundled tools.

## 6. The runtime layer

The runtime is a complete FHS userland with its own `ld.so`. The app is linked
against it and never against the host. The build tool can make a runtime from
four kinds of source:

| Source | Example | Use |
| --- | --- | --- |
| `oci://` | `oci://docker.io/library/archlinux:base-devel` | Pull any container image. Rootless pull, no Docker daemon |
| `tar://` | `tar://./rootfs.tar.zst` | A rootfs tarball from `debootstrap`, `pacstrap` or `apk` |
| `nix://` | `nix://./runtime.nix` | A Nix closure laid out as an FHS tree, reproducible |
| `cell://` | `cell://gnu-2026.1` | A prebuilt Cell runtime from a registry, by name and hash |

The reference runtime, `cell://gnu-<year>.<n>`, is Arch based because it
tracks new Mesa and glibc. Each runtime variant ships:

- glibc, gcc-libs, zlib, the usual base.
- Mesa: OpenGL, EGL, GBM, and the Vulkan ICDs `radv`, `anv`, `nvk` and
  `lavapipe`, plus VA-API and VDPAU drivers.
- The Vulkan loader, libglvnd, libdrm, wayland-client, libxkbcommon, Xlib and
  XCB, PipeWire and PulseAudio client libraries, SDL2 and SDL3, libudev,
  fontconfig, GTK and Qt platform plugins for Wayland and X11.
- Optional multilib: every item above as `lib32` too. Wine needs this.

A newer glibc inside the runtime than on the host is the safe direction. Host
libraries bound into the sandbox (host NVIDIA mode) load under the runtime's
`ld.so` and find every symbol version they need.

## 7. Isolation and grants

Default: nothing from the host is visible. The sandbox sees the merged root,
a private `/tmp`, a private `/run/user/<uid>`, a minimal `/dev`, a fresh
`/proc`, and read-only `/sys` subtrees needed for device enumeration.

Grants map to concrete binds, like Flatpak's `finish-args` and FlatImage's
`fim-perms`:

| Grant | What the launcher does |
| --- | --- |
| `display = [:wayland]` | Binds `$XDG_RUNTIME_DIR/$WAYLAND_DISPLAY` into the private runtime dir. Sets `WAYLAND_DISPLAY` |
| `display = [:x11]` | Binds `/tmp/.X11-unix/X<n>` for the `DISPLAY` in use. Writes a new Xauthority file with a wildcard address family so the cookie works inside the new UTS namespace. Sets `DISPLAY` and `XAUTHORITY` |
| `display = [:wayland, :x11_fallback]` | X11 only when no Wayland socket exists |
| `gpu { }` | Section 8 |
| `audio = [:pipewire, :pulse]` | Binds `pipewire-0` and `pulse/native` sockets. `:alsa` dev-binds `/dev/snd` |
| `input = [:gamepad]` | Dev-binds `/dev/input`, `/dev/hidraw*`, read-only `/run/udev` and the `/sys/class/input` and `/sys/devices` trees. `:uinput` adds `/dev/uinput` for Steam Input style remapping |
| `network = true` | Keeps the host network namespace. Binds `resolv.conf`, `hosts`, `nsswitch.conf` read-only |
| `network = false` | New network namespace with `lo` only |
| `dbus = :session` | Binds the session bus socket. `:filtered` adds an in-launcher proxy that allows only the names listed in `dbus_talk`. `:none` is the default |
| `home = :private` | Binds `$XDG_DATA_HOME/cell/apps/<id>/home` at the real `$HOME` path |
| `home = :portable` | Same, but the directory is `<file>.cell.d/home` beside the `.cell`. Saves travel with the file, like GameImage |
| `home = :host` | Binds the real home. Discouraged, allowed |
| `bind "ro" { host = "/mnt/games" guest = "/mnt/games" }` | Explicit extra binds, `ro`, `rw` or `dev` |
| `fonts = :host` | Binds host font directories read-only under `/run/host/fonts` and appends a fontconfig snippet. On NixOS the host path is `/run/current-system/sw/share/fonts` |
| `themes = :host` | Same for icons and cursor themes, via `XCURSOR_PATH` and `XDG_DATA_DIRS` |
| `shm = true` | Shares `/dev/shm`. Needed by some IPC-heavy apps |
| `pid = :private` | New pid namespace (default). `:host` keeps the host one for debuggers |

Seccomp: a baseline filter blocks namespace and mount syscalls, `keyctl`,
`ptrace` of other processes, and the usual container-escape set. Profiles are
`:default`, `:none`, or `:strict`.

Identity: one uid mapping (host uid to same uid). The app sees its real
username, synthesized into `/etc/passwd`. Build steps that need a root
identity run under a subuid range through `newuidmap` when the build host
provides one. When it does not, the build tool records ownership in the layer
metadata instead of chowning real files.

## 8. GPU: OpenGL and Vulkan

Two very different cases.

### 8.1 Mesa (AMD, Intel, Nouveau, software)

The kernel DRM interface is a stable userspace ABI. Mesa inside the runtime
drives any kernel. The launcher:

1. Dev-binds `/dev/dri`.
2. Binds `/sys/dev/char`, `/sys/devices` and `/sys/class/drm` read-only.
   libdrm walks these to find the PCI id of each render node.
3. Exposes the runtime's ICD files to the Vulkan loader and the EGL vendor
   files to libglvnd through the normal `/usr/share` paths.

Nothing comes from the host. This case works on NixOS with no extra steps.

### 8.2 NVIDIA proprietary

The userspace libraries must match the loaded kernel module exactly. The
launcher reads `/sys/module/nvidia/version`. The manifest picks a strategy:

| `nvidia =` | Behaviour |
| --- | --- |
| `:fetch` | Look in `$XDG_CACHE_HOME/cell/drivers/nvidia-<version>/`. If absent, download `NVIDIA-Linux-x86_64-<version>.run` from NVIDIA, with the same URL list the Flathub extension uses. The `.run` is a makeself archive: a shell header with a `skip=` line count, then a zstd stream (current drivers) or an xz stream (older ones). The launcher skips the header and unpacks the stream with Rust decoders. It keeps the GL, EGL, GLX, Vulkan, NVENC, CUDA and Wayland libraries plus the `32/` copies, and creates the SONAME links the installer would make. It writes the glvnd and Vulkan ICD JSON files. Nothing is redistributed, which keeps the licence question identical to Flatpak's |
| `:host` | Bind the host libraries. Discovery order: `/run/opengl-driver/lib` and `/run/opengl-driver-32/lib` on NixOS, then the host `ld.so.cache` and the well-known lib directories for `libnvidia-*` and `libcuda*`. Only the NVIDIA files are bound, into `/run/cell/gpu/lib`. The host glibc is never bound |
| `:auto` | `:fetch`, then `:host` when offline, then warn |
| `:none` | Software or Mesa only |

In every mode the launcher also:

- Dev-binds `/dev/nvidia*`, `/dev/nvidiactl`, `/dev/nvidia-modeset`,
  `/dev/nvidia-uvm` and `/dev/nvidia-caps`.
- Mounts the chosen driver directory at `/run/cell/gpu/lib` and
  `/run/cell/gpu/lib32`.
- Adds those paths to a generated `ld.so.conf` and regenerates
  `/etc/ld.so.cache` for the sandbox. It does not set `LD_LIBRARY_PATH`,
  because that variable leaks into every child and breaks Steam and Wine.
- Points `__EGL_VENDOR_LIBRARY_DIRS` and the Vulkan loader's
  `XDG_DATA_DIRS` entries at `/run/cell/gpu/share`.

### 8.3 Extras for games

- Vulkan layers: MangoHud, vkBasalt and the validation layers can live in the
  runtime. The manifest lists which layers to enable by default.
- Gamescope: an optional runtime component. `entry { gamescope = true }`
  wraps the command.
- Shader caches: `$XDG_CACHE_HOME/mesa_shader_cache` and the NVIDIA cache are
  bound from the app's state directory so they persist across runs.

## 9. Large applications

- Lazy mounting. Nothing is read until a page is touched. Start-up time does
  not depend on image size.
- Data layers with `compression = :none` for assets that are already packed.
  Reads are plain `pread` calls on the backing file, and with FUSE passthrough
  the kernel serves them without a context switch.
- Sidecar placement for multi-gigabyte layers and for publisher updates that
  change only the app layer.
- Content-addressed layers. A referenced runtime is shared across every game
  that uses it. Wine plus Mesa plus multilib is about one gigabyte, so sharing
  matters for a library of games.
- Extract fallback keyed by layer hash. A host without FUSE pays the cost once
  per layer, not once per run.
- Memory: the FUSE server keeps only the directory table and the block index
  in memory. Decompressed blocks go to the kernel page cache and are evicted
  normally.

## 10. The WCL build file

A build file is `cell.wcl` in a project directory. The schema is
`import <cell.wcl>`, which follows the vmlab pattern: WCL rejects unknown
blocks and fields, and `cell validate` applies the semantic rules.

```wcl
import <cell.wcl>

app "dev.wiltaylor.mygame" {
  name    = "My Game"
  version = "1.2.0"
  arch    = "x86_64"

  runtime {
    base      = "cell://gnu-2026.1"
    multilib  = true
    placement = :embedded
    packages  = ["wine", "winetricks", "gamescope", "mangohud", "lib32-mangohud"]
  }

  build {
    copy   { from = "./dist/game" to = "/app/game" }
    run    "winetricks --unattended corefonts vcrun2019"
    script "./build/prefix.ws"
  }

  layer "assets" {
    source      = "./dist/assets"
    mount       = "/app/game/assets"
    compression = :none
    placement   = :sidecar
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
    home    = :portable
    pid     = :private
    seccomp = :default
    bind "ro" { host = "/mnt/mods" guest = "/app/mods" }
  }

  integration {
    display = [:wayland, :x11_fallback]
    audio   = [:pipewire, :pulse]
    input   = [:gamepad, :uinput]
    dbus    = :none
    fonts   = :host
    gpu {
      enable = true
      nvidia = :auto
      vaapi  = true
    }
    desktop {
      icon       = "./dist/icon.png"
      categories = ["Game"]
      mime       = []
    }
  }

  sign { key = "./keys/release.ed25519" }
}
```

Templates follow the vmlab pattern. `template "wine-game" { ... }` beside the
`app` block carries the runtime, the integration and the Wine entry shape. A
game then writes `template = "wine-game"` and the few fields that differ.
The GameImage runners become templates: `wine-game`, `native-game`,
`retroarch`, `pcsx2`, `rpcs3`.

Schema sketch, in the vmlab style:

```wcl
symbol_set Placement   { embedded sidecar referenced }
symbol_set Compression { zstd none }
symbol_set HomeMode    { private portable host }
symbol_set PidMode     { private host }
symbol_set Seccomp     { default strict none }
symbol_set Display     { wayland x11 x11_fallback }
symbol_set Audio       { pipewire pulse alsa }
symbol_set Input       { gamepad uinput }
symbol_set Dbus        { none session filtered }
symbol_set Nvidia      { auto fetch host none }
symbol_set HostShare   { none host }

@document
type CellDoc {
  @children("app")      apps:      list<App>
  @children("template") templates: list<Template>
}

@block("app")
type App {
  @doc("Reverse-DNS app id; names the state directory and the desktop entry")
  @inline(0) id: utf8
  name:     utf8
  version:  utf8
  @options(["x86_64", "aarch64"]) arch: utf8
  template: utf8?
  @child("runtime")     runtime:     Runtime?
  @child("build")       build:       Build?
  @children("layer")    layers:      list<Layer>
  @child("entry")       entry:       Entry
  @child("isolation")   isolation:   Isolation?
  @child("integration") integration: Integration?
  @child("sign")        sign:        Sign?
}

@block("runtime")
type Runtime {
  @doc("oci://, tar://, nix:// or cell:// source")
  base:      utf8
  @default(false) multilib: bool?
  @default(:embedded) placement: Placement?
  @doc("Packages installed with the base's own package manager during build")
  packages:  list<utf8>?
}

@block("layer")
type Layer {
  @inline(0) name: utf8
  source:      utf8
  mount:       utf8
  @default(:zstd) compression: Compression?
  @default(:embedded) placement: Placement?
}

@block("isolation")
type Isolation {
  @default(false)    network: bool?
  @default(:private) home:    HomeMode?
  @default(:private) pid:     PidMode?
  @default(:default) seccomp: Seccomp?
  @default(false)    shm:     bool?
  @children("bind")  binds:   list<Bind>
}

@block("integration")
type Integration {
  display: list<Display>?
  audio:   list<Audio>?
  input:   list<Input>?
  @default(:none) dbus: Dbus?
  dbus_talk: list<utf8>?
  @default(:none) fonts:  HostShare?
  @default(:none) themes: HostShare?
  @child("gpu")     gpu:     Gpu?
  @child("desktop") desktop: Desktop?
}

@block("gpu")
type Gpu {
  @default(true)  enable: bool?
  @default(:auto) nvidia: Nvidia?
  @default(true)  vaapi:  bool?
  layers: list<utf8>?
}
```

Semantic rules for `cell validate`:

- `multilib = true` needs a runtime that offers `lib32` packages.
- `:x11_fallback` needs `:wayland` in the same list.
- `dbus_talk` needs `dbus = :filtered`.
- A `sidecar` layer needs a name that is a valid file name.
- `home = :portable` and `placement = :referenced` on the same app is a
  warning: the file is portable but it needs a network once.
- Every `copy.from`, `layer.source`, `script` and `icon` path must exist.

## 11. Tooling

Two binaries.

`cell` (the build tool, dynamic or static, runs on the author's machine):

- `cell validate` checks the file.
- `cell build -o MyGame.cell` resolves the runtime, runs the build steps
  inside the same sandbox the launcher uses, packs the layers, writes the
  manifest, signs, and appends everything to the launcher.
- `cell inspect MyGame.cell` prints the manifest and the layer table.
- `cell extract`, `cell verify`, `cell runtime pull`, `cell runtime list`.
- `cell run <file>` runs a `.cell` from a `noexec` mount.

The launcher, inside every `.cell` file, takes control commands through an
environment variable so they never collide with app arguments:

- `CELL_CMD=info` prints the host probe, the chosen tier, and the grants.
- `CELL_CMD=shell` opens a shell inside the sandbox with the same grants.
- `CELL_CMD=extract` writes the layers to a directory.
- `CELL_CMD=verify` checks hashes and the signature.
- `CELL_ALLOW=network,home` and `CELL_DENY=gpu` are one-run overrides.

User overrides persist in `$XDG_CONFIG_HOME/cell/overrides/<id>.wcl` and
use the same `isolation` and `integration` blocks as the build file.

Desktop integration is opt-in: `CELL_CMD=install` writes the `.desktop` file
and icon into the user's XDG directories, with `Exec` pointing at the file's
current location.

## 12. Implementation

- Language: Rust, targets `x86_64-unknown-linux-musl` and
  `aarch64-unknown-linux-musl`. No C dependencies in the launcher.
- Namespaces and mounts: `rustix` and `nix`. Direct syscalls, no `bwrap`.
- FUSE: `fuser` with the low-level API; passthrough when the kernel supports
  it.
- squashfs: `backhand` for reading, and for writing in the build tool.
- zstd: `zstd` crate, static.
- Hashing and signing: `blake3`, `ed25519-dalek`.
- Manifest: `serde` with `ciborium` for CBOR.
- WCL: the `wcl_lang` crate in the build tool only.
- xz decoding for NVIDIA `.run` archives: `lzma-rs`, pure Rust.
- Build steps run in the same sandbox code path as the launcher, with a
  writable upper directory that becomes the app layer. The build tool and the
  launcher share one crate for the sandbox.

Launcher size target: under 3 MiB. Start-up target: under 100 ms to `exec`
on tier 0, not counting the first NVIDIA fetch.

## 13. Risks and open questions

- Ubuntu's AppArmor user namespace restriction is the most common real-world
  blocker. The launcher must give a clear, copyable fix.
- NVIDIA `.run` URLs change for datacenter branches. The launcher needs a
  small URL table that the build tool can refresh, and `:host` as the
  fallback.
- Host NVIDIA libraries may link against a newer libstdc++ than the runtime
  ships. The runtime should track a current gcc-libs and the launcher should
  prefer `:fetch`.
- Wine wants case-insensitive paths for some games. FlatImage uses CIOPFS.
  A FUSE case-folding layer on the upper directory is possible but costs
  speed. Defer. Wine handles most cases itself.
- Portals. Flatpak's file chooser and screen-cast portals need a filtered
  session bus and the `.flatpak-info` convention. A later version can speak
  that convention so `xdg-desktop-portal` treats a Cell like a Flatpak.
- Abstract-socket-only X servers. The launcher must bind the socket file, so
  it needs `/tmp/.X11-unix/X<n>` to exist. Nearly every X server creates it.
- Signing policy. Who verifies, and what happens on failure: warn by default,
  refuse with `CELL_REQUIRE_SIGNATURE=1`.
- Should the launcher accept DwarFS layers in version 1? No. Add the codec
  when image size on real games shows the gain.

## 14. Milestones

1. Launcher: trailer, layer table, FUSE squashfs mount, userns, overlayfs,
   `exec`. Run a static hello world from an Alpine rootfs.
2. Runtime: the `cell://gnu` reference runtime with Mesa. Wayland and X11
   grants. Run `vkcube` and `glxgears` on AMD and Intel.
3. NVIDIA `:fetch` and `:host`. Test on NixOS and Arch.
4. Build tool: WCL schema, `cell validate`, `cell build` with `oci://` and
   `cell://` runtimes.
5. Audio, input, multilib, Wine template. Run a Steam game through Wine.
6. Sidecar layers, FUSE passthrough, referenced runtimes, fallbacks.
7. Overrides, desktop install, signing.

## 15. Implementation notes (2026-10-19)

The first implementation lives in this directory. Where it departs from the
sections above:

- **Two user namespaces at run time.** The launcher maps the real uid to
  root in an outer namespace, makes every mount with that root, and enters
  an inner namespace that maps root back to the real uid just before `exec`.
  Mount targets inside root-owned image directories need the outer root; the
  app still runs as the user. The build tool instead maps root to the real
  uid and the subordinate ranges from `/etc/subuid`, so package managers
  can chown.
- **The pid namespace comes from `clone`**, because `unshare(CLONE_NEWPID)`
  may only be called once per process and the build tool runs many steps.
- **The sandbox dies with its launcher** through `PR_SET_PDEATHSIG` on pid 1
  of the pid namespace.
- **An X11 grant shares the host IPC namespace**, because X clients hand
  MIT-SHM segments to the X server. Flatpak's `--share=ipc` has the same
  reason.
- **The symbols `none` is reserved in WCL**, so the schema uses `raw` for
  uncompressed layers and `off` for D-Bus and NVIDIA.
- **The manifest holds the layer table**; there is no separate table.
- **NVIDIA `:fetch` is implemented** with `ureq` on rustls, which compiles
  for musl through the zig wrapper. `:auto` fetches, then falls back to host
  libraries. The driver archives are zstd today, not xz; both are read. The
  tar holds no symlinks, so the launcher reads `DT_SONAME` from each library
  and creates the links itself. The launcher grew to 3 MiB.
- **`oci://` bases are pulled natively**: token auth, manifest lists,
  digest-verified blobs, layer whiteouts, and a rootfs cache keyed by the
  manifest digest. `docker://` keeps the `docker export` path.
- **The FUSE layer squashes ownership** to the mounting user. The outer
  namespace maps one uid, so any other uid or gid stored in the image is
  unmapped, and even root in the namespace cannot create a mount point in
  such a directory. An image whose `/` was not owned by uid 0 and gid 0
  failed to start before this. The build tool keeps real ownership.
- **One instance owns the state.** The launcher takes an `flock` on
  `instance.lock` in the state directory. A second instance of the same app
  runs with a throwaway overlay upper and shares the home bind.
- **`:referenced` layers are files on disk, not downloads.** The build tool
  writes `<layer>-<hash16>.layer` beside the output and reuses an existing
  one with the same name. The launcher resolves it through a search path.
  The name carries the hash; `CELL_CMD=verify` checks the full BLAKE3.
- **The build tool runs inside a cell.** `cell-builder.cell` carries the
  static tool and launcher on an Alpine runtime. Without `newuidmap` the
  build namespace maps one id and runs as fake root; pacman's `alpm`
  download user is turned off inside build sandboxes for that reason.
  `$PWD` in `entry.cwd` and in bind paths lets a tool cell start where the
  user is.
- **Desktop and Steam registration live in the launcher.** `install`
  writes the freedesktop entry, icon, MIME types, URL schemes and one
  desktop action per named command. `steam-add` edits Steam's binary
  `shortcuts.vdf` and writes the grid art under the crc32-derived app id.
  The art is PNG bytes in the manifest, declared in the build file.
- **Hub, launcher app and identity moved to wiltaylor/cell-hub** on
  2026-10-20. The notes below describe them as first built here.
- **Identity is a keypair, not an account.** Launcher, CLI, web UI and
  hub each hold an Ed25519 profile. Pairing signs `cell-pair:v1:<hub
  key>:<code>`; sign-in signs `cell-login:v1:<hub key>:<nonce>` and gets a
  session token. The export file is PBKDF2-SHA256 plus AES-256-GCM so a
  browser reads it with WebCrypto. Peers pair through a hub by code; the
  hub relays presence and game invites. No passphrase on the local key.
- **Pid 1 of the sandbox is a small init**, as in bubblewrap. The entry
  point is its child. The init forwards signals, reaps orphans, and waits
  for daemons such as `wineserver` to finish after the entry point exits.
  Without that, the registry Wine writes at shutdown was lost.
- **Opaque-directory markers from the build overlay are not carried** into
  the app layer, because the FUSE server does not serve xattrs. Whiteouts
  (character devices 0:0) are carried.

## 16. Windows (2026-10-21)

The format ports as is: a `.cell.exe` is the Windows launcher (a PE built
for `x86_64-pc-windows-gnu`) with the same layers, CBOR manifest and
trailer appended. A PE loader ignores trailing data like an ELF loader
does. The manifest gained `app.os` (`linux` by default) so a launcher can
refuse a file for the other OS, and the build tool chooses the launcher
from it. `cell build` runs on Linux for both.

What Windows lacks is the runtime: no unprivileged user namespaces, no
FUSE, no overlayfs. ThinApp and Cameyo answer that with an API-hooking
engine inside the process. Cell answers it with WinFsp, which is what a
FUSE server would be on Windows, and a fallback that needs nothing.

### 16.1 Tiers

| Tier | Needs | Lazy | Writes |
| --- | --- | --- | --- |
| 0 | WinFsp installed (one MSI, admin once, no reboot) | yes | union volume, copy-up into `%LOCALAPPDATA%\cell\apps\<id>\upper` |
| 1 | nothing | no, extract once per layer-hash set | into the extracted tree at `...\apps\<id>\root` |

The launcher never links WinFsp. `cell_core::winfsp` finds
`winfsp-x64.dll` through the installer's registry key or the Program Files
path, loads it with `LoadLibraryW` and resolves the dozen entry points it
uses. So the same `.exe` runs on a host without WinFsp and picks tier 1.
The struct layouts (`FSP_FSCTL_VOLUME_PARAMS`, `FILE_INFO`, `DIR_INFO`,
the interface table) are declared by hand from the WinFsp 2.x headers with
size assertions.

### 16.2 The union file system

`cell_core::unionfs` is overlayfs in user mode:

- Lowers are the squashfs layers, read with the same reader the Linux FUSE
  server uses. The root stack (runtime, app) mounts at `/`; data layers
  mount at their path, and the directories above a mount point exist
  synthetically when no layer has them.
- The upper is an NTFS directory. The upper wins; directories merge; a
  file in a higher layer hides the layers below it.
- A delete of a lower name writes `.cellwh.<name>` beside it in the upper.
  A whiteout plus an upper entry of the same name is opaque: the lower is
  hidden even for a directory, which is what "remove, then recreate" needs.
- Any open with a write-ish access mask (`FILE_WRITE_DATA`, `APPEND`,
  `WRITE_EA`, `WRITE_ATTRIBUTES`, `DELETE`, `WRITE_DAC`, `WRITE_OWNER`)
  copies the lower file up first, then opens the upper copy. A directory
  opened for write is created in the upper. Renaming a lower directory
  copies its whole tree up.
- Lower names are matched without case through a per-directory folded
  index, because the volume reports itself case-insensitive like NTFS. The
  build tool refuses layers that have two names differing only in case,
  or names Windows cannot hold.
- One security descriptor serves every file: full access for SYSTEM,
  administrators, Everyone and app containers. The sandbox is not the ACL.
- The volume calls itself `NTFS`, which WinFsp's FAQ asks for so that a
  program run as administrator from the volume starts.
- Directory listings use WinFsp's directory-buffer helpers so markers and
  partial reads work without more code here.

The mount point is `%LOCALAPPDATA%\cell\mnt\<id>`, a directory WinFsp
creates and removes. A second instance of the same app mounts at
`<id>-<pid>` with a throwaway upper, as on Linux. Seen in the lab: a mount
made by one logon is visible to SYSTEM in another session, so nothing is
per-session about it.

### 16.3 The launcher

`cell-launcher/src/windows.rs`. It joins a job object with
kill-on-close before spawning, so every descendant dies with the
launcher, which is the Linux init guarantee. Ctrl-C is left to the app.
`$CELL_ROOT` and `$PWD` expand in arguments, `cwd` and `env` values;
`exec[0]` and `cwd` that start with `/` map onto the root.
`home = :private | :portable` sets `USERPROFILE`, `APPDATA`,
`LOCALAPPDATA`, `TEMP` and `TMP` to a per-app home. That is an environment
convention, not an enforcement.

### 16.4 The sandbox (2026-10-21)

`cell-launcher/src/appcontainer.rs`. `CreateAppContainerProfile` needs
no administrator; the profile name is `cell.<id>`. The launcher adds
inheritable ACEs for the container SID on the app's state directory, on
the mount point's parent and on every `bind` host path, builds the
capability list from `network`, and spawns with
`PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES`. Three things learned the
hard way:

- An empty capability list must be a null pointer, or `CreateProcessW`
  fails with `ERROR_INVALID_PARAMETER`.
- A handle the child inherits is access-checked against the container
  token, so the terminal's or an agent's stdio handles never arrive. The
  launcher creates its own pipes with a descriptor that names
  `ALL APPLICATION PACKAGES`, hands them to the child and pumps them to
  its own stdout and stderr.
- WinFsp creates each volume device with
  `D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GRGX;;;WD)`, which no AppContainer can
  pass, so every open died in the I/O manager before the file system saw
  it. `IoCreateDeviceSecure` lets device class security in the registry
  override that default. `CELL_CMD=setup` (`setup.rs`) writes
  `D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GRGX;;;WD)(A;;GA;;;AC)` under
  `HKLM\SYSTEM\CurrentControlSet\Control\Class\{b48171c3-dd50-4852-83a3-344c50d93b17}\Properties`
  through `SeRestorePrivilege`, after a UAC prompt; the next mount creates
  a device that app packages can open. The launcher reads the device
  descriptor through a volume handle after mounting and, when app
  packages are missing, runs the sandboxed app from the extract tier
  with a notice. Read and execute is not enough for the device: writes
  need full access there too.

The manifest field is `isolation { sandbox = :appcontainer | :off }`,
default on; `CELL_SANDBOX=off` overrides per run. Linux ignores it.

### 16.5 Not done, in order

1. **Registry.** `RegLoadAppKey` for the layer hive and the upper hive,
   plus registry hooks to merge them. Captured at build time from a diff.
2. **Build-time capture.** `run` steps need a Windows build host: run the
   installer under the launcher with a scratch upper and freeze the upper
   into the app layer. This is the ThinApp and Cameyo capture, declarative.
3. Desktop and Steam integration, the PE icon resource, Authenticode
   signing (unsigned EXEs with a large overlay trip SmartScreen), 32-bit
   and ARM64 launchers.

### 16.6 Licence note

WinFsp is GPLv3 with a FLOSS exception that allows linking the DLL and
shipping the installer from software under a FLOSS licence, with a notice
in the UI and documentation, and not "linked or distributed with
proprietary software". Cell is MIT. A commercial game packed as a cell
needs the question answered before it ships; the author offers a
commercial licence.
