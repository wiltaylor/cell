# Getting the tool

The packaged builder needs no installation. Copy `examples/dist/cell-builder.cell`
from the Cell repository (or build it with `just example-build cell-builder`) to a
directory of your choice and run it: `./cell-builder.cell build cell.wcl -o app.cell`.
It carries the static `cell` tool, the launcher it embeds in every output, and an
Alpine runtime with `mksquashfs` and `curl`. Set `CELL_BUILDER=/path/to/cell-builder.cell`
so this skill finds it. It can see your home, the directory you run it from, `/tmp`,
`/mnt`, `/media` and `/run/media`; put build inputs and outputs there.

The installed tool: from the repository root run `just all-build`, then use
`target/release/cell`. It needs `mksquashfs`, `newuidmap` with an `/etc/subuid` entry,
and finds `cell-launcher` under `target/x86_64-unknown-linux-musl/release/`.

Build-time hosts need unprivileged user namespaces and `/dev/fuse`. Run-time hosts
need the same, and nothing else: the launcher is static.
