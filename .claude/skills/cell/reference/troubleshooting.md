# Troubleshooting

| Message | Cause | Fix |
| --- | --- | --- |
| `parse error: expected symbol name or '}', found 'none'` | `none` is a reserved word | Use `:raw` (compression) or `:off` (dbus, nvidia) |
| `schema errors: ... unknown field` | Field or block name is wrong | Check `reference/wcl-surface.md` |
| `missing schema import` | First line is not `import <cell.wcl>` | Add it |
| `runtime.base ... must start with oci://, docker://, tar:// or dir://` | Wrong scheme | Use one of those |
| `target not found: <pkg>` | Package name not in the base's repositories | Fix the name; set `multilib = true` for `lib32-*`; add a `setup` step that installs from another source |
| `step failed with exit status N` | A `run` or package step failed | Read its output above; steps run as root with network |
| `no entry for <user> in /etc/subuid` | Installed tool needs subordinate ids | Use `cell-builder.cell`, or add a range with `usermod` |
| `cannot find cell-launcher` | Installed tool cannot find the launcher binary | `CELL_LAUNCHER=<path>` or use the packaged builder |
| `cannot create a user namespace` | Host blocks unprivileged user namespaces | `sysctl kernel.unprivileged_userns_clone=1`; on Ubuntu 23.10+ `sysctl kernel.apparmor_restrict_unprivileged_userns=0` |
| `/dev/fuse is missing` | FUSE module not loaded | `modprobe fuse` |
| `layer <name> is referenced, not embedded: put <file> beside this cell` | A shared runtime file is missing | Copy the named `.layer` next to the cell, or set `CELL_LAYER_PATH`, or put it in `~/.local/share/cell/layers` |
| `another instance of <id> is running` | Second instance of the same app | Normal; it runs with a throwaway overlay |
| `mount overlay ... Resource busy` | A stale sandbox still holds the state dir | `pgrep -a -f <name>.cell`; kill the launcher's child (`kill -KILL $(pgrep -P <launcher pid>)`) |
| App exits at once, no window | No display grant, or the toolkit needs X11 | Add `display = [:x11, :wayland]` |
| `MESA-EGL: warning: pci id for fd ...: 10de:28a0, driver (null)` | Mesa probing an NVIDIA node | Harmless on hybrid laptops |
| `DirectSoundCreate8 ... 0x80040154` in an old Wine prefix | Registry lost before the init fix | Delete the prefix in the state dir (ask first) and run again |
| Files shown as owned by `nobody` or by the user | Expected: the namespace maps one uid | Nothing; permission bits still apply |

