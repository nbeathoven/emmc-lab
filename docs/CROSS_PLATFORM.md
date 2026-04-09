# Cross-Platform Plan (Linux, macOS, Windows)

This document describes how to move `emmc-lab` from Linux-first to true cross-platform support while preserving current behavior on Raspberry Pi Linux.

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
