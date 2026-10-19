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

# Build one example into examples/<name>/<name>.cell
[group('example')]
example-build name: all-build
	cd examples/{{name}} && ../../target/release/cell build cell.wcl -o {{name}}.cell

# Show the host probe and grants of a built example
[group('example')]
example-info name:
	CELL_CMD=info ./examples/{{name}}/{{name}}.cell

# Run a built example
[group('example')]
example-run name *args:
	./examples/{{name}}/{{name}}.cell {{args}}
