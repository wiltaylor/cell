# Cell

A portable, sandboxed application format for Linux. One `.cell` file holds a
static launcher, a complete userland with its own glibc, the application, and
a manifest that says what the application may touch on the host.

- The launcher is a static musl binary. It runs on any glibc, including NixOS.
- The application runs in its own user, mount, pid and (optionally) network
  namespace. Nothing on the host is visible unless the manifest grants it.
- Wayland, X11, PipeWire, PulseAudio, gamepads, D-Bus and host fonts are
  grants. OpenGL and Vulkan come from Mesa inside the runtime. The NVIDIA
  userspace is matched to the host kernel module.
- Layers are squashfs images mounted lazily over FUSE from inside the user
  namespace. No `fusermount`, no `bwrap`, no setuid helper, no copy to `/tmp`.
- A `cell.wcl` file describes the build. `cell build` turns it into a `.cell`.

See `DESIGN.md` for the full design and `schema/cell.wcl` for the build file
surface.

## Build

```sh
just all-build
```

This needs: a Rust toolchain with the `x86_64-unknown-linux-musl` target,
`zig` (musl C compiler for the zstd and ring sources), `mksquashfs`
(squashfs-tools), `newuidmap` and an `/etc/subuid` entry for the build
namespace. `oci://` bases are pulled from the registry by the tool itself;
`docker://` uses a local Docker instead.

## Make a cell

```sh
cd examples/tuxracer
../../target/release/cell validate cell.wcl
../../target/release/cell build cell.wcl -o tuxracer.cell
./tuxracer.cell
```

`cell build` runs every step as root in a user namespace mapped with your
subordinate ids, like rootless podman. The runtime layer (base image plus
packages) is cached in `~/.cache/cell/runtimes` by content key.

## Run

```sh
./app.cell                      # run the entry point
CELL_CMD=info ./app.cell        # host probe, grants, layers
CELL_CMD=shell ./app.cell       # a shell inside the sandbox
CELL_CMD=verify ./app.cell      # check layer hashes
CELL_COMMAND=glxinfo ./app.cell # run a named alternative command
CELL_NVIDIA=host ./app.cell     # force one NVIDIA strategy (host, fetch, none)
```

Writable state lives in `$XDG_DATA_HOME/cell/apps/<id>/`: an overlay upper
directory for everything the app writes outside its home, and `home/` which
is bound at the real `$HOME` path. `home = :portable` moves that beside the
`.cell` file instead.

## Layout

```
crates/cell-core      file format, squashfs reader, FUSE server, sandbox
crates/cell-launcher  the static launcher (musl)
crates/cell           the build tool (reads cell.wcl)
schema/cell.wcl       the WCL schema for build files
examples/             alpine-test, tuxracer, wine-doom
tools/musl-cc         zig wrapper used as the musl C compiler
```

## Status

Working: launcher, layers, overlay, an init at pid 1, grants for display,
audio, input, dbus, fonts, GPU (Mesa; NVIDIA fetched from NVIDIA to match
the kernel module, or bound from the host), build tool with native
`oci://`, `docker://`, `tar://` and `dir://` bases, data layers, sidecars,
named commands.

Not yet: seccomp filters, D-Bus filtering, desktop file install, signing,
the `proot` fallback for hosts without user namespaces.

## Tested on 2026-10-19

Host: CachyOS, kernel 7.2, KDE on Wayland, Intel Iris Xe plus an NVIDIA
RTX 4060 with driver 615.71.09.

| Example | Size | Result |
| --- | --- | --- |
| `examples/alpine-test` | 5 MiB | Entry point runs as the real user; FUSE layers, overlay, private `/dev`, `/tmp`, `/run` |
| `examples/tuxracer` | 269 MiB | OpenGL 4.6 and Vulkan 1.4 on Intel through the runtime's Mesa; NVIDIA through the fetched 615.71.09 userspace (17 s first run) and through host libraries; a full race renders at 2560x1600; an audio stream reaches the host |
| `examples/wine-doom` | 766 MiB | Wine prefix created on first run; Minesweeper and Chocolate Doom (Freedoom) run through Wine with wined3d and DirectSound audio |

Notes:

- Every file in the image shows as owned by the user. The launcher's
  namespace maps one uid, so the FUSE layer reports that uid for every
  file. Permission bits still apply as usual.
- An X11 grant keeps the host IPC namespace (MIT-SHM), as Flatpak does.
- The first instance of an app owns its overlay upper directory. A second
  instance of the same app runs with a throwaway upper and prints a notice.
  Both share the home directory.
