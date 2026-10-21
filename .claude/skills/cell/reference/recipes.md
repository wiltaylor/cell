# Recipes

Every recipe is a complete build file. Copy one, then change the marked lines.

## native-game: a Linux game from distribution packages

```wcl
import <cell.wcl>

app "dev.example.mygame" {                       // CHANGE
  name    = "My Game"                            // CHANGE
  version = "1.0"

  runtime {
    base      = "oci://archlinux:base"
    placement = :referenced                      // one runtime file shared by cells in the output dir
    packages  = [
      "mesa", "vulkan-icd-loader", "vulkan-intel", "vulkan-radeon", "vulkan-swrast",
      "libva-mesa-driver", "mesa-utils", "vulkan-tools",
      "libpulse", "pipewire", "libxkbcommon-x11",
      "mygame",                                  // CHANGE: the game's package
    ]
  }

  entry {
    exec = ["mygame"]                            // CHANGE
    command "glxinfo"    { exec = ["glxinfo", "-B"] }
    command "vulkaninfo" { exec = ["vulkaninfo", "--summary"] }
  }

  isolation {
    network = false
    home    = :private
  }

  integration {
    display = [:wayland, :x11_fallback]          // SFML 2 and old SDL need [:x11, :wayland]
    audio   = [:pipewire, :pulse]
    input   = [:gamepad]
    fonts   = true
    gpu { enable = true nvidia = :auto }
    desktop {                                    // CELL_CMD=install
      icon       = "./art/icon.png"              // CHANGE: PNG, square
      comment    = "One line for the menu"       // CHANGE
      categories = ["Game"]
      protocols  = ["mygame"]                    // CHANGE or remove: mygame:// links
    }
    steam {                                      // CELL_CMD=steam-add
      portrait = "./art/portrait.png"            // CHANGE: 600x900
      wide     = "./art/wide.png"                // CHANGE: 920x430
      hero     = "./art/hero.png"                // CHANGE: 1920x620
      logo     = "./art/logo.png"                // CHANGE: transparent PNG
      tags     = ["Cell"]
    }
  }
}
```

Make the art with ImageMagick from screenshots when the game ships none:
`magick shot.png -resize 1920x1200^ -gravity center -extent 1920x620 art/hero.png`,
`-extent 920x430` for `wide`, `-extent 600x900` for `portrait`.

A game shipped as files instead of a package: add `build { copy { from = "./dist/game"
to = "/app/game" } }` and `entry { exec = ["/app/game/run.sh"] cwd = "/app/game" }`.
Big asset directories go in a `layer "assets" { source = "./dist/assets" mount =
"/app/game/assets" compression = :raw placement = :sidecar }` block so they are not
recompressed and stay out of the main file.

## wine-game: a Windows game through Wine

```wcl
import <cell.wcl>

app "dev.example.winegame" {                     // CHANGE
  name    = "Wine Game"                          // CHANGE
  version = "1.0"

  runtime {
    base     = "oci://archlinux:base"
    multilib = true
    packages = [
      "wine", "wine-mono",
      "mesa", "lib32-mesa", "vulkan-icd-loader", "lib32-vulkan-icd-loader",
      "vulkan-intel", "lib32-vulkan-intel", "vulkan-radeon", "lib32-vulkan-radeon",
      "libpulse", "lib32-libpulse", "pipewire", "lib32-pipewire",
      "lib32-gnutls", "lib32-libxcomposite", "lib32-libxinerama", "lib32-libxrandr",
      "lib32-libxcursor", "lib32-libxi", "lib32-alsa-plugins",
      "ttf-dejavu", "mesa-utils", "vulkan-tools",
    ]
  }

  build {
    copy { from = "./dist/game" to = "/app/game" }   // CHANGE: the game's files
  }

  entry {
    exec = ["wine", "/app/game/Game.exe"]        // CHANGE
    cwd  = "/app/game"
    env "WINEDEBUG" { value = "-all" }
    command "winecfg" { exec = ["wine", "winecfg"] }
  }

  isolation {
    network = false
    home    = :private                           // the Wine prefix lives in this home
  }

  integration {
    display = [:x11, :wayland]                   // Wine needs X11
    audio   = [:pipewire, :pulse]
    input   = [:gamepad]
    fonts   = true
    gpu { enable = true nvidia = :auto }
  }
}
```

The first run creates the Wine prefix (20 to 60 s). Wine saves its registry a few
seconds after the game exits; the launcher's init waits for that.

## cli-tool: a command-line program that works on the user's files

```wcl
import <cell.wcl>

app "dev.example.mytool" {                       // CHANGE
  name    = "My tool"                            // CHANGE
  version = "1.0"

  runtime {
    base     = "oci://alpine:3.22"
    packages = ["ca-certificates"]               // CHANGE: what the tool needs
  }

  build {
    copy { from = "./target/x86_64-unknown-linux-musl/release/mytool" to = "/app/bin/mytool" }  // CHANGE
  }

  entry {
    exec = ["/app/bin/mytool"]                   // CHANGE
    cwd  = "$PWD"                                // start where the user is
  }

  isolation {
    network = true                               // CHANGE: false if the tool is offline
    home    = :host                              // the tool works on the user's files
    bind "rw" { host = "$PWD" guest = "$PWD" }   // the current directory, wherever it is
    bind "rw" { host = "/tmp" guest = "/tmp" }
  }

  integration {
    gpu { enable = false }
  }
}
```

Arguments after the cell name go to the program: `./mytool.cell --flag file`.

## windows-app: a Windows program as a `.cell.exe`

The build runs on Linux and needs `target/x86_64-pc-windows-gnu/release/cell-launcher.exe`
(`just launcher-build-windows`, or `CELL_LAUNCHER_WINDOWS=<path>`). The app must be
relocatable, which games and portable apps are: the files appear under a mount point,
never under `C:\Program Files`. There is no runtime block, no `run` step and no grant;
copy the installed files in and point the entry at them.

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

`cell build cell.wcl -o game.cell` writes `game.cell.exe`. On a Windows host with WinFsp
installed it mounts the layers at `%LOCALAPPDATA%\cell\mnt\<id>` and runs the entry;
without WinFsp it extracts them once. `home = :private` points `USERPROFILE`, `APPDATA`,
`LOCALAPPDATA` and `TEMP` at a per-app home. Nothing sandboxes the process yet, so
`network` and `bind` have no effect on Windows. The build tool rejects file names
Windows cannot hold and names that differ only in case.

## Sharing one runtime between many games

Give each game the same `runtime` block (same base, packages, setup, multilib) and
`placement = :referenced`, then build them all into one directory. The first build
writes `runtime-<hash>.layer`; the others reuse it and are a few MiB each. A `template`
block in one file can hold the shared `runtime` block for apps in that file.
