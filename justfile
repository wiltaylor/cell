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

# Build the Windows launcher (x86_64-pc-windows-gnu, needs mingw-w64)
[group('windows')]
launcher-build-windows:
	cargo build --release -p cell-launcher --target x86_64-pc-windows-gnu

# Build the Windows test program and its DLL
[group('windows')]
win-test-build:
	cd examples/win-test && cargo build --release --target x86_64-pc-windows-gnu
	cd examples/win-test/dll && cargo build --release --target x86_64-pc-windows-gnu
	python3 -c "import struct,pathlib; p=pathlib.Path('examples/win-test/dist/data/big.bin'); p.exists() or p.write_bytes(b''.join(struct.pack('<I',i) for i in range((16<<20)//4)))"

# Build a Windows example into examples/dist/<name>.cell.exe (file is the .wcl name without extension)
[group('windows')]
example-build-windows name file="cell": tool-build launcher-build-windows
	mkdir -p examples/dist
	cd examples/{{name}} && ../../target/release/cell build {{file}}.wcl -o ../dist/{{name}}.cell

# Copy a built Windows example into the lab VM and run it there (lab/windows must be up)
[group('windows')]
win-run name *args:
	cd lab/windows && vmlab cp ../../examples/dist/{{name}}.cell.exe win:C:/cell/{{name}}.cell.exe
	cd lab/windows && vmlab exec --user vmlab win -- C:/cell/{{name}}.cell.exe {{args}}
