# emmc-lab

`emmc-lab` is a single-binary Rust CLI for Linux storage testing and I/O diagnostics that combines:

- eMMC and file-backed workload testing
- exact operation-count random and sequential I/O runs
- live procfs-based I/O diagnostics
- eMMC health snapshots through direct EXT_CSD access with `mmc-utils` fallback
- JSON, CSV, and HTML reporting
- an SSH-friendly interactive menu plus direct automation commands

Current release: `v0.2.6`

License: [MIT](LICENSE)

## What It Does

`emmc-lab` has two primary jobs:

1. Stress and characterize eMMC or file-backed storage with reproducible workloads.
2. Show which processes, files, directories, and devices are using I/O on the system.

It is designed as one application, not a loose bundle of scripts. The default command opens an SSH-first managed terminal console with a stable operator frame:

- header with app version, host, environment, and current screen
- clear path/context blocks plus a current-item detail pane
- explicit key hints repeated in the footer
- read-only and state-changing actions described in plain language before execution
- side-by-side panels when width allows, with stacked fallback on smaller terminals

## How It Works

At a high level, a run goes through these stages:

1. Load a YAML profile or collect answers through the interactive wizard.
2. Validate the workload, addressing range, stop condition, and safety requirements.
3. Refuse destructive raw-device writes unless the target is a block device, not mounted, not the active root device, and explicitly confirmed.
4. Execute the workload through a native Rust engine using explicit-offset `pread` and `pwrite`.
5. Record exact completed read and write counts, byte counts, interval samples, latency percentiles, and error counters.
6. Optionally collect health snapshots before and after the run.
7. Optionally collect live diagnostic sampler data during the run to flag outside I/O interference.
8. Persist session results and export terminal, JSON, CSV, and HTML summaries.

Diagnostic mode works in two levels:

- `diag sample`: lightweight procfs/sysfs sampling of per-process and device activity
- `diag trace`: best-effort deep-trace entry point that explains unavailable tracing support and can fall back to sampler mode

## Main Features

- interactive main menu with wizard-based test creation
- embedded quick presets with names and descriptions for common file, partition, raw-device, and focused logical-sector runs
- direct commands for automation
- YAML saved profiles
- exact `op_count` stopping
- logical sector range targeting and byte-range targeting
- separate logical syscall bytes and storage-layer bytes in diagnostics
- best-effort `fdinfo`-backed file context and unattributed device-I/O totals in diagnostics
- health snapshots with direct EXT_CSD parsing and graceful `mmc-utils` fallback when raw access is unavailable
- decoded eMMC CID identity capture in health snapshots and saved reports
- boot-partition sector verification for concurrent boot-read/user-partition-write stress runs
- lightweight persistence without an external database
- optional embedded SQLite feature flag
- Debian-family self-deploy installer, including Raspberry Pi OS

## Important Limitation

Logical range targeting is LBA-based only.

`emmc-lab` can target exact logical sector ranges and exact operation counts such as `1,000,000` random writes in a selected logical range, but it does **not** claim fixed physical NAND cell targeting. Reports and docs repeat this explicitly.

## Safety

Raw-device writing is destructive.

`emmc-lab` refuses destructive raw-device runs unless all of these are true:

1. the target is a block device
2. the target is not mounted
3. the target is not the active root device
4. the requested range fits the device
5. the user passes `--i-understand-this-will-destroy-data`

## Platform Support

`emmc-lab` is Linux-first.

- Runtime support: Linux systems with `/proc`, `/sys`, and standard block-device interfaces
- Common targets: Raspberry Pi OS, Debian, Ubuntu, and similar modern Linux distributions
- Build hosts: Linux and macOS are both practical for development builds
- Optional Linux-only integrations: `mmc-utils`, tracing/eBPF, direct block-device access

Current portability model:

- the core app is not Raspberry Pi-specific
- eMMC health features are most useful on systems that expose eMMC devices
- diagnostics and raw-device workflows are Linux-only
- Windows is not a supported runtime target today

## Quick Start

Open the interactive menu:

```bash
emmc-lab
```

The interactive menu now groups workload entry under `Run Workload`, with:

- `Quick Presets`
- `Guided Custom Run`
- `Saved Profiles`
- `Repeat Last Run`

The quick preset list includes `Concurrent Partition Stress` for platforms where boot-partition reads and user-partition writes need to be exercised together. It reads a selected boot partition range, writes a selected non-root user partition, captures CID and health before/after, and compares an md5 checksum of the boot range after the run.

The managed console uses a small fixed key vocabulary throughout:

- `Up/Down` or `j/k`: move selection
- `Enter`: open the highlighted item
- `Esc`: go back without applying changes
- `q`: quit from selector screens
- `Ctrl-C`: immediate termination

Run the wizard directly:

```bash
emmc-lab wizard
```

Run a saved profile:

```bash
emmc-lab run --profile examples/profiles/file-randread.yaml
```

Run live diagnostics for 60 seconds:

```bash
emmc-lab diag sample --duration 60 --interval-ms 1000
```

Check device health:

```bash
emmc-lab health --device /dev/<device>
```

## Usage Examples

Interactive menu:

```bash
emmc-lab
```

Open the guided wizard directly:

```bash
emmc-lab wizard
```

Run a saved profile:

```bash
emmc-lab run --profile ~/.config/emmc-lab/profiles/test.yaml
```

Run `1,000,000` logical reads against one logical sector from the menu:

1. `emmc-lab`
2. `1. Run Workload`
3. `1. Quick Presets` or `2. Guided Custom Run`
4. if using presets, pick `Focused Logical Range Presets` -> `One Sector 1M Reads`
5. if using the wizard, choose `Range` -> `4=one sector`
6. choose the target device
7. `Target sector`: enter the logical sector number
8. finish the preset flow or wizard and run

Run `1,000,000` logical writes inside a selected logical sector range:

```bash
emmc-lab run --profile examples/profiles/million-randwrite-range.yaml --i-understand-this-will-destroy-data
```

Run the live I/O monitor with 1 second refresh:

```bash
sudo emmc-lab diag monitor --interval-ms 1000
```

Run a timed sampler for 60 seconds:

```bash
emmc-lab diag sample --duration 60 --interval-ms 1000
```

Run deep trace for 30 seconds, with sampler fallback if tracing is unavailable:

```bash
sudo emmc-lab diag trace --duration 30 --fallback-to-sampler
```

Export an HTML report for a saved session:

```bash
emmc-lab export --session 20260404T052158-d71f295942c342398805398f3a3a4beb --format html
```

List detected block devices:

```bash
emmc-lab list-devices
```

Run environment and capability checks:

```bash
emmc-lab doctor
```

## Debian-Family Quick Install

The repo includes an optional helper installer under `rpi/` for Raspberry Pi OS and other `apt`-based Linux distributions.

Local install from a checkout:

```bash
./rpi/install.sh
```

GitHub self-deploy install:

```bash
curl -fsSL https://raw.githubusercontent.com/nbeathoven/emmc-lab/main/rpi/install.sh | bash
```

The installer:

- checks required Debian packages and skips installed ones
- installs missing required packages
- installs optional `mmc-utils` and `fio` by default
- installs Rust if `cargo` is missing
- clones or updates the repo if needed
- builds `emmc-lab` in release mode
- installs the binary into `/usr/local/bin`
- prints the final prompt needed to launch the app

After install, run:

```bash
emmc-lab
```

## Build

Requirements:

- Linux with `/proc`, `/sys`, and standard block-device interfaces
- Rust toolchain (`cargo`, `rustc`) unless using the installer

Build manually:

```bash
cargo build --release
sudo install -m 0755 target/release/emmc-lab /usr/local/bin/emmc-lab
```

Optional embedded SQLite build:

```bash
cargo build --release --features sqlite
```

## Project Layout

- `src/`: application source
- `examples/`: consolidated shipped reference assets
- `examples/profiles/`: example YAML profiles
- `examples/reports/`: sample JSON and HTML report artifacts
- `rpi/`: optional Debian-family install scripts and notes, including Raspberry Pi OS
- `tests/`: integration coverage

## Reporting and Storage

Each session is stored without an external database server.

Default outputs include:

- `session.json`
- `intervals.jsonl`
- optional JSON export
- optional CSV export
- optional HTML export

Reports include:

- session metadata
- target and workload configuration
- exact operation count or runtime
- logical range details
- performance and latency summaries
- errors and verification results
- health before and after when available
- notes on logical-vs-physical limitations
- interference notes when diagnostics are captured during testing

## Dependencies and Fallbacks

Mandatory runtime inputs:

- Linux procfs and sysfs
- writable output directories

Optional runtime tools:

- `mmc-utils` as the final EXT_CSD fallback path
- kernel tracing or eBPF support for deep tracing
- `fio` for optional comparison workflows

If an optional feature is unavailable, `emmc-lab` reports that clearly and falls back or disables that capability instead of failing silently.

## Versioning and Releases

- Crate version: `0.2.6`
- License: `MIT`
- Changelog: [CHANGELOG.md](CHANGELOG.md)
- GitHub releases: [Releases](https://github.com/nbeathoven/emmc-lab/releases)

## Project Standards

The repository includes the standard public-code publishing files:

- [LICENSE](LICENSE)
- [CONTRIBUTING.md](CONTRIBUTING.md)
- [SECURITY.md](SECURITY.md)
- [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md)

## Reference Assets

The repo keeps shipped examples in one place:

- profiles under [examples/profiles](/Users/nima/Documents/Develop/eMMC/examples/profiles)
- sample report artifacts under [examples/reports](/Users/nima/Documents/Develop/eMMC/examples/reports)
