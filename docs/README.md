# emmc-lab

`emmc-lab` is a single Rust CLI application for Linux storage testing and I/O diagnostics that combines:

- eMMC and file-backed workload testing
- live procfs-based I/O diagnostics
- best-effort deep-trace fallback handling
- eMMC health snapshots via `mmc-utils` when available
- JSON, CSV, and HTML reporting
- interactive menu plus direct CLI automation

## Build

Requirements:

- Rust toolchain (`cargo`, `rustc`)
- Linux with `/proc`, `/sys`, and standard block-device interfaces

Build:

```bash
cargo build --release
sudo install -m 0755 target/release/emmc-lab /usr/local/bin/emmc-lab
```

## Platform Support

`emmc-lab` is Linux-first, not Raspberry Pi-only.

- Runtime target: Linux
- Works best on modern distributions with `/proc`, `/sys`, and standard block-device access
- Common fit: Raspberry Pi OS, Debian, Ubuntu, and similar Linux distributions
- Build hosts: Linux and macOS are both practical for development builds
- Optional integrations such as `mmc-utils` and tracing remain Linux-specific

Windows is not a supported runtime target today.

## Debian-Family Self-Deploy Install

The repository includes an optional helper installer under `rpi/` for Raspberry Pi OS and other `apt`-based Linux systems.

Local install from a checkout:

```bash
./rpi/install.sh
```

Self-deploy install from GitHub:

```bash
curl -fsSL https://raw.githubusercontent.com/nbeathoven/emmc-lab/main/rpi/install.sh | bash
```

The installer in [rpi/install.sh](/Users/nima/Documents/Develop/eMMC/rpi/install.sh):

- checks required Debian packages and skips those already installed
- installs missing required packages
- installs optional `mmc-utils` and `fio` by default
- installs Rust if `cargo` is missing
- clones or updates the GitHub checkout when needed
- builds `emmc-lab`
- installs the binary into `/usr/local/bin`
- prints the final command prompt needed to start the app

Optional build features:

```bash
cargo build --release --features sqlite
```

## Runtime Dependency Policy

Mandatory runtime inputs:

- Linux procfs and sysfs
- a writable output directory

Optional runtime tools:

- `mmc-utils` for EXT_CSD health
- kernel tracing/eBPF support for deep tracing
- `fio` only if you later add or compare against a generated fio job

If optional features are unavailable, `emmc-lab` reports that clearly and continues with supported functionality instead of crashing.

## Safety

Raw-device writes are destructive.

`emmc-lab` refuses destructive raw-device runs unless all of these are true:

1. The target is a block device.
2. The target is not mounted.
3. The target is not the active root device.
4. The target range fits the device.
5. The user explicitly confirms with `--i-understand-this-will-destroy-data`.

The interactive wizard also warns before destructive raw-device runs.

## Important Limitation

Logical range targeting is LBA-based only.

`emmc-lab` can target exact logical sector ranges and exact operation counts such as `1,000,000` random writes in a selected logical range, but it does **not** claim fixed physical NAND cell targeting. Reports repeat this note explicitly.

## Interactive Usage

Launch the menu:

```bash
emmc-lab
```

Main menu:

1. Run Workload
2. Diagnostics
3. Device Health
4. Reports
5. Settings
6. Help
7. Exit

The wizard supports:

- back
- cancel
- save as profile
- run now

The wizard asks for:

1. test mode
2. target path
3. test type
4. logical range addressing mode
5. stop condition
6. block size
7. queue depth
8. worker count
9. random seed
10. direct I/O
11. verification
12. durability mode
13. health telemetry
14. diagnostic capture during test
15. profile name

It prints:

- a saved YAML profile path if chosen
- a printable summary
- the equivalent direct command

## Direct Commands

Run a saved profile:

```bash
emmc-lab run --profile examples/file-randread.yaml
```

Launch the wizard directly:

```bash
emmc-lab wizard
```

Health check:

```bash
emmc-lab health --device /dev/<device>
```

Live sampler for 60 seconds:

```bash
emmc-lab diag sample --duration 60 --interval-ms 1000
```

Deep trace for 30 seconds:

```bash
emmc-lab diag trace --duration 30
```

Report view:

```bash
emmc-lab report --session 20260404T010101-abc123
```

Export HTML:

```bash
emmc-lab export --session 20260404T010101-abc123 --format html
```

List devices:

```bash
emmc-lab list-devices
```

Doctor checks:

```bash
emmc-lab doctor
```

## Required Example Scenarios

Guided test setup:

```bash
emmc-lab wizard
```

Exact `1,000,000` write run in a selected logical sector range:

```bash
emmc-lab run --profile examples/million-randwrite-range.yaml --i-understand-this-will-destroy-data
```

Run test plus diagnostics together:

```bash
emmc-lab run --profile examples/combined-test-with-diagnostics.yaml
```

Health check for a block device:

```bash
emmc-lab health --device /dev/<device>
```

## Reporting

Per-session output defaults to JSON storage under the app data directory.

Stored artifacts:

- `session.json`
- `intervals.jsonl`
- optional exported `.json`
- optional exported `.html`
- optional exported `*_summary.csv`, `*_intervals.csv`, `*_processes.csv`

Each test report includes:

- session metadata
- target and workload config
- exact op count or runtime
- logical sector range
- performance summary
- latency summary
- errors
- verification results
- health before and after
- logical-vs-physical limitation notes
- possible interference notes when diagnostic capture is enabled

Each diagnostic report includes:

- top readers
- top writers
- top files
- top directories
- device totals
- unresolved path count
- dropped event count
- exact-vs-best-effort attribution notes

## Deep Trace Notes

The current codebase degrades gracefully when deep tracing is unavailable. If tracing prerequisites are missing, it explains why and can fall back to sampler mode.

When deep tracing is not available, procfs sampler mode remains lightweight and continues to separate:

- logical syscall bytes from `/proc/<pid>/io` `rchar` and `wchar`
- storage-layer bytes from `/proc/<pid>/io` `read_bytes` and `write_bytes`

These values are never merged into a single mislabeled number.

## Backend Notes

The workload engine currently runs through native Rust plus `pread` and `pwrite` with explicit offsets.

`emmc-lab doctor` reports whether `io_uring` appears available on the host, but this codebase currently executes workloads through the fallback backend rather than a bundled `io_uring` engine implementation.

## Doctor Coverage

`emmc-lab doctor` checks:

- kernel version
- io_uring availability
- `O_DIRECT` viability
- target device visibility
- mount safety visibility
- writable output directory
- `mmc-utils` availability
- deep trace availability
- effective permission level
- procfs access

## Test Coverage

The repository includes unit and integration tests for:

- profile parsing
- menu input parsing
- exact op-count logic
- sector range validation
- alignment validation
- metrics aggregation
- health parser fallback
- procfs parsing helpers
- path resolution helpers
- safety guard behavior
- file-mode execution
- combined test and diagnostics session handling
- JSON, CSV, and HTML export generation
