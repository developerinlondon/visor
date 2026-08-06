# Visor — build, test, and quality gate targets.
#
# Usage:
#   make build           Build all crates (workspace + visor-init static musl)
#   make release-mac     Build release + codesign for macOS (HVF entitlement)
#   make codesign-check  Verify release binary has HVF entitlement
#   make test            Run full test suite
#   make check           Run all quality gates
#   make kernel          Rebuild guest kernel from source
#   make clean           Clean all build artifacts
#   make run             Build and start the daemon in foreground

INIT_TARGET := x86_64-unknown-linux-musl

.PHONY: build test check kernel clean run fmt lint release-mac codesign-check

## Build everything: workspace (debug) + visor-init (release, static musl)
build:
	cargo build --workspace
	cargo build -p visor-init --release --target $(INIT_TARGET)

## Build optimized release (requires CC=clang for FIPS)
release:
	CC=clang AWS_LC_FIPS_SYS_CC=clang cargo build --workspace --release
	cargo build -p visor-init --release --target $(INIT_TARGET)

## Build release + codesign for macOS (HVF entitlement)
release-mac: release
	codesign --sign - --entitlements entitlements.plist --force ./target/release/visor
	@echo "==> Release binary codesigned with HVF entitlement"

## Verify the release binary has the HVF entitlement
codesign-check:
	@codesign -d --entitlements - ./target/release/visor 2>/dev/null | grep -q com.apple.security.hypervisor && echo "==> HVF entitlement: OK" || (echo "ERROR: Binary missing HVF entitlement. Run: make release-mac" && exit 1)

## Run all tests
test:
	cargo test --workspace

## Run all quality gates
check: fmt lint test
	@echo "==> All quality gates passed"

## Format check (dprint)
fmt:
	dprint check

## Lint (clippy + cargo check)
lint:
	cargo check --workspace
	cargo clippy --workspace --tests -- -D warnings

## Rebuild guest kernel from source (~30s incremental, ~15min clean)
kernel:
	./crates/visor-kernel/scripts/build-kernel.sh
	cargo clean -p visor-kernel

## Clean all build artifacts
clean:
	cargo clean

## Build and start daemon in foreground
run: build
	./target/debug/visor start --foreground
