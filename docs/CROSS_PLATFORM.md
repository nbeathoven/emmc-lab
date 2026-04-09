# Cross-Platform Plan (Linux, macOS, Windows)

This document describes how to move `emmc-lab` from Linux-first to true cross-platform support while preserving current behavior on Raspberry Pi Linux.

## Publisher Rollout (How to Apply This Plan)

If you publish binaries/releases, apply this plan in phases so users always get a working Linux build while other platforms are added safely.

### Phase 0: Baseline and branch setup

1. Create a feature branch for platform work.
2. Add a CI matrix with Linux/macOS/Windows `cargo check --workspace`.
3. Keep Linux integration tests required; allow non-Linux jobs to run portable checks first.

### Phase 1: Internal adapter extraction (no user-facing behavior change)

1. Introduce `platform/` modules and traits (see "Recommended Refactor").
2. Move existing Linux logic into the Linux adapter.
3. Keep command output unchanged on Linux.

### Phase 2: Non-Linux compile support

1. Add macOS/Windows adapter stubs returning capability-disabled results for Linux-only features.
2. Gate raw-device paths on adapter capability checks.
3. Ensure non-Linux commands fail gracefully only when a Linux-only feature is requested.

### Phase 3: Runtime hardening on macOS/Windows

1. Enable file-based workload runs end-to-end on both platforms.
2. Add platform-specific terminal handling where needed.
3. Add integration tests for file-mode profiles on each OS runner.

### Phase 4: Release and communication

1. Publish release notes with a support matrix (Linux full; macOS/Windows core).
2. Update docs with feature availability by platform.
3. Provide platform-specific install artifacts:
   - Linux tarball/package
   - Windows zip (`emmc-lab.exe`)
   - macOS tarball/pkg

### Suggested publisher commands

```bash
# verify formatting and compile
cargo fmt --all -- --check
cargo check --workspace
cargo test --workspace

# produce release binary
cargo build --release
```

## Current State

The project currently relies on Linux/Unix-specific interfaces in several places:

- `/proc` and `/sys` for diagnostics and system discovery
- `std::os::unix::*` traits/imports
- direct `libc` calls for terminal and low-level I/O paths
- `mmc-utils` integration (Linux eMMC tooling)

As a result, the current runtime support should be considered Linux-first.

## Target Support Levels

- **Tier 1 (full): Linux**
  - existing behavior, including raw-device safety checks and diagnostics
- **Tier 2 (core): macOS, Windows**
  - file-based workloads
  - profile handling and reporting
  - no hard failure when Linux-only features are unavailable

## Recommended Refactor

## 1) Introduce a platform adapter boundary

Create a `platform` module with submodules:

- `platform/linux.rs`
- `platform/windows.rs`
- `platform/macos.rs`
- `platform/mod.rs` (trait definitions + `cfg` dispatch)

Suggested trait boundaries:

- `SystemProbe` (kernel/host/process visibility)
- `DeviceEnumerator` (block/file target enumeration)
- `TerminalControl` (TTY capability, raw mode, dimensions)
- `IoPrimitives` (open/read/write/sync flags)

This keeps core workload and report logic platform-agnostic.

## 2) Keep Linux behavior as the reference implementation

Move existing Linux logic into `platform/linux.rs` first with minimal logic changes.

## 3) Add Windows/macOS fallbacks

For unsupported Linux-only data sources:

- return `None`/empty collections
- include clear capability messages in doctor/diagnostics output
- preserve command success when safe fallback is possible

## 4) Remove unconditional Unix imports from shared modules

Replace direct `std::os::unix::*` usage in shared modules with adapter calls or `cfg(unix)` gated implementations.

## 5) Gate raw-device features by capability

Keep raw-device destructive paths disabled unless the platform adapter confirms safe support.

## Build & CI Matrix

Use a build matrix that validates compilation on all OS targets:

```yaml
strategy:
  matrix:
    os: [ubuntu-latest, windows-latest, macos-latest]
steps:
  - uses: actions/checkout@v4
  - uses: dtolnay/rust-toolchain@stable
  - run: cargo check --workspace
  - run: cargo test --workspace
```

If some runtime tests are Linux-only, split tests into:

- portable tests (all OS)
- Linux integration tests (`#[cfg(target_os = "linux")]`)

## Packaging Guidance

- Linux: distro package or static binary + install script
- Windows: zip release with `emmc-lab.exe` and sample configs
- macOS: signed tarball/pkg if distribution requires notarization

## Definition of Done for Cross-Platform

- `cargo check --workspace` passes on Linux/macOS/Windows
- portable tests pass on Linux/macOS/Windows
- Linux-only diagnostics and raw-device behavior remain unchanged on Linux
- unsupported features on non-Linux are explicit and non-crashing
