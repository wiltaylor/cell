# Cell — a portable, sandboxed application format for Linux.

[default, private]
main:
	@just --list

# Build the static musl launcher
[group('build')]
launcher-build:
	cargo build --release -p cell-launcher --target x86_64-unknown-linux-musl

# Build the `cell` build tool
[group('build')]
tool-build:
	cargo build --release -p cell

# Build everything
[group('build')]
all-build: launcher-build tool-build

# Run the test suite and clippy
[group('check')]
workspace-check:
	cargo test --workspace
	cargo clippy --workspace --all-targets -- -D warnings

# Validate one example build file
[group('example')]
example-validate name: tool-build
	./target/release/cell validate examples/{{name}}/cell.wcl

# Build one example into examples/dist/<name>.cell (referenced layers are shared there)
[group('example')]
example-build name: all-build
	mkdir -p examples/dist
	cd examples/{{name}} && ../../target/release/cell build cell.wcl -o ../dist/{{name}}.cell

# Show the host probe, grants and layer files of a built example
[group('example')]
example-info name:
	CELL_CMD=info ./examples/dist/{{name}}.cell

# Run a built example
[group('example')]
example-run name *args:
	./examples/dist/{{name}}.cell {{args}}

# Build the hub web UI (apps/hub-web/dist, embedded into cell-hub)
[group('hub')]
hub-web-build:
	pnpm install
	pnpm --dir apps/hub-web build

# Build the cell-hub server with the embedded web UI
[group('hub')]
hub-build: hub-web-build
	cargo build --release -p cell-hub

# Run a hub on port 8787 with ./hub-data and a dev token
[group('hub')]
hub-serve token='devtoken': hub-build
	CELL_HUB_TOKEN={{token}} ./target/release/cell-hub serve --listen 0.0.0.0:8787 --data ./hub-data

# Open the terminal console against a local hub
[group('hub')]
hub-console url='http://127.0.0.1:8787' token='devtoken':
	CELL_HUB_TOKEN={{token}} ./target/release/cell-hub console --url {{url}}

# Build the desktop launcher (Tauri)
[group('launcher')]
launcher-app-build:
	pnpm install
	pnpm --dir apps/launcher build
	cargo build --release -p cell-launcher-app

# Run the desktop launcher (builds it first only when the binary is missing)
[group('launcher')]
launcher:
	@test -x target/release/cell-launcher-app || just launcher-app-build
	./target/release/cell-launcher-app

# Rebuild the desktop launcher and run it
[group('launcher')]
launcher-app-run: launcher-app-build
	./target/release/cell-launcher-app

# Install the launcher for this user: ~/.local/bin and an app-menu entry
[group('launcher')]
launcher-install:
	@test -x target/release/cell-launcher-app || just launcher-app-build
	mkdir -p ~/.local/bin ~/.local/share/applications ~/.local/share/icons/hicolor/128x128/apps
	cp target/release/cell-launcher-app ~/.local/bin/cell-launcher-app
	cp apps/launcher/src-tauri/icons/128x128.png ~/.local/share/icons/hicolor/128x128/apps/cell-launcher.png
	printf '[Desktop Entry]\nType=Application\nName=Cell Launcher\nComment=Your library of portable games\nExec=%s/.local/bin/cell-launcher-app\nIcon=cell-launcher\nTerminal=false\nCategories=Game;\n' "$HOME" > ~/.local/share/applications/cell-launcher.desktop
	-update-desktop-database ~/.local/share/applications 2>/dev/null
	@echo "installed: run 'cell-launcher-app' or find Cell Launcher in the app menu"

# Run the launcher with hot reload (Vite + Tauri dev)
[group('launcher')]
launcher-app-dev:
	pnpm --dir apps/launcher tauri dev
