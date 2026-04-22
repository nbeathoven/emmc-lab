# emmc-lab: Endurance Test Features — Agent Work Order

> **Audience**: AI coding agent. Read this entire document before writing any code.
> **Codebase**: Rust, Linux-only, single binary. v0.2.6, 13 k lines across 16 modules.
> **Read first**: `AGENT_ARCHITECTURE.md` for the full module map and coding conventions.

---

## Context

The target test scenario is: write to a user partition and read from a boot partition
concurrently, for millions of iterations, across operator-controlled power cycles.
At the end of each power-cycle the operator manually verifies boot partition integrity
before resuming. emmc-lab must provide the I/O engine, integrity tooling, checkpointing,
and CQHCI diagnostic telemetry to support this workflow.

---

## What Does Not Exist Today

| Capability | Missing from |
|------------|-------------|
| Concurrent I/O on more than one device | `engine.rs`, `profile.rs` |
| Boot partition unlock (`force_ro`) | entire codebase |
| Region checksum (MD5 / SHA-256) persisted across sessions | entire codebase |
| Per-iteration checkpoint saved to disk | `storage.rs` |
| CQHCI enabled/disabled detection | `health.rs`, `system.rs` |
| MMC host controller state from sysfs | `system.rs` |

---

## Task 1 — `src/boot_partition.rs` (new file)

Create this file and declare it in `lib.rs` as `pub mod boot_partition;`.

### 1.1 Public API

```rust
use std::path::{Path, PathBuf};
use anyhow::Result;

/// RAII guard that unlocks a boot partition for the duration of its lifetime
/// and re-locks it on drop. Locking is always restored — even on panic.
pub struct BootPartitionGuard {
    device_name: String,   // bare name, e.g. "mmcblk0boot0"
    was_readonly: bool,    // state before we touched it
}

impl BootPartitionGuard {
    /// Read /sys/class/block/{name}/force_ro.
    /// If 1, write "0" to unlock. Record original state for Drop.
    /// If already 0, no-op (was_readonly = false, nothing to restore).
    pub fn unlock(device: &Path) -> Result<Self>;
}

impl Drop for BootPartitionGuard {
    fn drop(&mut self) {
        if self.was_readonly {
            let path = Self::force_ro_path(&self.device_name);
            let _ = std::fs::write(path, "1\n");
        }
    }
}

impl BootPartitionGuard {
    fn force_ro_path(device_name: &str) -> PathBuf {
        PathBuf::from(format!("/sys/class/block/{}/force_ro", device_name))
    }
}

/// Returns true if the path's basename matches mmcblk{N}boot{M}.
pub fn is_boot_partition(device: &Path) -> bool;

/// Given a parent device name (e.g. "mmcblk0"), return the two boot partition
/// paths if they exist under /sys/class/block/.
pub fn list_boot_partitions(parent_name: &str) -> Vec<PathBuf>;

/// Size in bytes: read /sys/class/block/{name}/size (sectors) × 512.
pub fn boot_partition_size_bytes(device: &Path) -> Option<u64>;
```

### 1.2 Integration points

**`engine.rs`** — in the function that opens a target fd (currently `open_fd()`):
before calling `libc::open`, call `is_boot_partition(path)`. If true, call
`BootPartitionGuard::unlock(path)?` and store the guard in the run-context struct
(must outlive the fd). On write access to a boot partition, also check
`profile.safety.allow_boot_partition_write` — if false, return an error before
any fd is opened.

**`profile.rs`** — add to `SafetyConfig`:
```rust
pub allow_boot_partition_write: bool,  // default: false
```

**`system.rs` `doctor()`** — add two checks: (a) are `mmcblk0boot0` / `mmcblk0boot1`
present in `/sys/class/block/`? (b) what is the current `force_ro` value for each?
Report both as `DoctorCheck` entries.

### 1.3 Unit tests

```rust
#[test]
fn test_is_boot_partition() {
    assert!(is_boot_partition(Path::new("/dev/mmcblk0boot0")));
    assert!(is_boot_partition(Path::new("/dev/mmcblk1boot1")));
    assert!(!is_boot_partition(Path::new("/dev/mmcblk0")));
    assert!(!is_boot_partition(Path::new("/dev/mmcblk0p1")));
    assert!(!is_boot_partition(Path::new("/dev/sda")));
}
```

---

## Task 2 — `src/integrity.rs` (new file)

Create this file and declare it in `lib.rs` as `pub mod integrity;`.
Add `md-5 = "0.10"` and `sha2 = "0.10"` to `Cargo.toml`.

### 2.1 Types

```rust
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntegrityBaseline {
    pub device: PathBuf,
    pub region_offset_bytes: u64,
    pub region_length_bytes: u64,  // 0 = entire device
    pub algorithm: ChecksumAlgorithm,
    pub checksum_hex: String,
    pub region_size_actual: u64,   // bytes actually read
    pub captured_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ChecksumAlgorithm { Crc32, Md5, Sha256 }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntegrityVerification {
    pub baseline: IntegrityBaseline,
    pub verified_at: DateTime<Utc>,
    pub verified_checksum_hex: String,
    pub matches: bool,
    pub note: String,
}
```

### 2.2 Functions

```rust
/// Read `length` bytes from `device` at `offset` (O_DIRECT, aligned buffer)
/// and compute checksum. length == 0 → use BLKGETSIZE64 to read entire device.
/// Returns IntegrityBaseline with checksum_hex and region_size_actual filled in.
pub fn compute_region_checksum(
    device: &Path,
    offset_bytes: u64,
    length_bytes: u64,
    algorithm: ChecksumAlgorithm,
) -> Result<IntegrityBaseline>;

/// Re-read the same region as baseline and compare checksums.
/// Returns Ok(result) with matches=false on mismatch.
/// Returns Err only on I/O failure (device unreadable).
pub fn verify_against_baseline(
    baseline: &IntegrityBaseline,
    note: &str,
) -> Result<IntegrityVerification>;
```

### 2.3 O_DIRECT implementation detail

The read buffer must be aligned to the device's `logical_block_size`.
Use `posix_memalign` — the same pattern already exists in `engine.rs` for direct I/O.
Read in chunks of at most 1 MiB. Feed each chunk into the running hash state before
reading the next chunk so peak memory stays bounded regardless of partition size.

BLKGETSIZE64 ioctl number: `0x80081272` (`_IOR(0x12, 114, u64)`).

### 2.4 CLI — extend `cli.rs`

Add an `Integrity` subcommand with three sub-subcommands:

```
emmc-lab integrity capture --device /dev/mmcblk0boot0 --algorithm sha256 --session <ID>
emmc-lab integrity verify  --device /dev/mmcblk0boot0 --session <ID> --note "after cycle 3"
emmc-lab integrity history --session <ID>
```

`capture` calls `compute_region_checksum()`, appends to `SessionRecord.integrity_baselines`,
saves the session, and prints the checksum.

`verify` calls `verify_against_baseline()` using the most recent baseline for that device,
appends to `SessionRecord.integrity_verifications`, saves the session, and prints PASS/FAIL.

`history` loads the session and prints a table of all baselines and verifications.

### 2.5 SessionRecord additions (`storage.rs`)

```rust
pub struct SessionRecord {
    // ... all existing fields unchanged ...
    #[serde(default)]
    pub integrity_baselines: Vec<IntegrityBaseline>,
    #[serde(default)]
    pub integrity_verifications: Vec<IntegrityVerification>,
}
```

`#[serde(default)]` is mandatory — existing session JSON files have no these fields and
must still deserialise without error.

### 2.6 Auto-capture from profile

Add to `profile.rs`:

```rust
pub struct IntegrityRegionConfig {
    pub device: PathBuf,
    pub offset_bytes: u64,
    pub length_bytes: u64,   // 0 = entire device
    pub algorithm: ChecksumAlgorithm,
    pub capture_before_run: bool,
}

// In Profile:
#[serde(default)]
pub integrity: Vec<IntegrityRegionConfig>,
```

In `app.rs`, before calling `execute_profile()`, iterate `profile.integrity` and call
`compute_region_checksum()` for each region where `capture_before_run == true`.
Store results in `SessionRecord.integrity_baselines` and save the checkpoint immediately
so the baseline survives even if the run is interrupted.

### 2.7 Unit tests

```rust
#[test]
fn test_checksum_algorithm_round_trip() {
    // Write known bytes to a tempfile, compute each algorithm,
    // verify the hex matches a known-good value.
}

#[test]
fn test_verify_detects_mismatch() {
    // Compute baseline on tempfile, modify one byte, verify → matches == false.
}
```

---

## Task 3 — Iteration Checkpointing (`storage.rs`)

### 3.1 New struct

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestCheckpoint {
    pub session_id: String,
    pub iteration: u64,
    pub elapsed_secs: u64,
    pub integrity_baselines: Vec<IntegrityBaseline>,
    pub interval_stats: Vec<IntervalStats>,
    pub notes: Vec<String>,
    pub saved_at: DateTime<Utc>,
}
```

### 3.2 New functions

```rust
/// Atomic write: serialise to {session_dir}/checkpoint.json.tmp,
/// then std::fs::rename() to checkpoint.json.
/// rename() is atomic on Linux local filesystems.
pub fn save_checkpoint(paths: &AppPaths, cp: &TestCheckpoint) -> Result<PathBuf>;

/// Return Some(checkpoint) if checkpoint.json exists for this session, else None.
pub fn load_checkpoint(paths: &AppPaths, session_id: &str) -> Result<Option<TestCheckpoint>>;
```

### 3.3 Profile config

```rust
// In Profile:
#[serde(default)]
pub checkpointing: CheckpointConfig,

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CheckpointConfig {
    pub enabled: bool,
    pub every_n_iterations: u64,  // 0 = disabled
}
```

### 3.4 Engine integration (`engine.rs`)

In the main worker-loop tick (the same place `IntervalStats` are sampled), check if
`profile.checkpointing.enabled` and whether `ops_completed % every_n_iterations == 0`.
If so, send a checkpoint request through a `std::sync::mpsc::Sender<CheckpointRequest>`
to a dedicated checkpoint writer thread. The writer thread calls `save_checkpoint()`.
This keeps the hot I/O path free of file I/O latency.

The checkpoint holds the `integrity_baselines` already captured before the run started
(passed in at run start) plus `interval_stats` accumulated so far.

---

## Task 4 — Concurrent Multi-Partition Workload (`profile.rs`, `engine.rs`)

This is the largest change. Implement it last.

### 4.1 Profile additions

```rust
// In profile.rs — add alongside existing `target: TargetConfig`:
#[serde(default)]
pub secondary_targets: Vec<SecondaryTarget>,

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecondaryTarget {
    pub target: TargetConfig,
    pub workload: WorkloadConfig,
    #[serde(default = "default_worker_count")]
    pub worker_count: usize,
}
```

Secondary targets are independent workloads running in parallel with the primary. They
share the primary's stop signal but have their own fd, own op counters, and own stats.

### 4.2 Engine additions

Add to `RunSummary`:

```rust
#[serde(default)]
pub secondary_summaries: Vec<SecondaryRunSummary>,

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecondaryRunSummary {
    pub target: PathBuf,
    pub ops_completed: u64,
    pub bytes_completed: u64,
    pub errors: u64,
}
```

In `execute_profile()`:

1. After opening the primary fd and spawning primary workers, iterate `profile.secondary_targets`.
2. For each `SecondaryTarget`, call `BootPartitionGuard::unlock()` if needed, open a separate
   fd, spawn `secondary_target.worker_count` threads pointing at that fd.
3. Pass the same `Arc<AtomicBool>` stop signal to all secondary worker threads so they halt
   when the primary stops.
4. Join all secondary thread handles after joining primary handles.
5. Collect per-target stats into `SecondaryRunSummary` and attach to `RunSummary`.

Secondary workers use the same read/write loop already in the engine. The only difference
is they do not own the stop condition — they only observe it.

### 4.3 Example profile YAML

```yaml
name: boot-read-userpart-write

target:
  mode: raw_device
  path: /dev/mmcblk0

secondary_targets:
  - target:
      mode: raw_device
      path: /dev/mmcblk0boot0
    workload:
      test_type: read
      block_size_bytes: 204800
      queue_depth: 1
    worker_count: 1

workload:
  test_type: write
  block_size_bytes: 204800
  worker_count: 1
  queue_depth: 1
  exact_op_count: 1500000

integrity:
  - device: /dev/mmcblk0boot0
    offset_bytes: 0
    length_bytes: 0
    algorithm: sha256
    capture_before_run: true

checkpointing:
  enabled: true
  every_n_iterations: 500

safety:
  destructive_confirmation_required: true
  allow_boot_partition_write: false
```

---

## Task 5 — CQHCI Detection (`system.rs`, `health.rs`)

### 5.1 EXT_CSD additions (`health.rs`)

The raw EXT_CSD bytes are already read into `ext_csd_raw: Option<[u8; 512]>`.
Add two fields to `EmmcHealthSnapshot` (and to the full `ExtCsdDecoded` struct when that
is built per `REQUIREMENTS.md` REQ-3):

```rust
pub cmdq_support: Option<u8>,  // EXT_CSD byte 308 — bit 0 = CMDQ capable
pub cmdq_depth: Option<u8>,    // EXT_CSD byte 307 — actual depth = value + 1
```

Parse them in `parse_ext_csd()` alongside the existing byte reads.

### 5.2 New struct and function (`system.rs`)

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CqhciStatus {
    pub mmc_host: String,
    pub cmdq_enabled: bool,
    pub cmdq_depth: Option<u8>,
    pub driver_name: Option<String>,
    pub kernel_version: String,
    pub risk: CqhciRisk,
    pub warning: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CqhciRisk {
    None,    // CQHCI not present or not enabled
    Low,     // Enabled but kernel ≥ 5.15 (NXP addressed it here)
    High,    // Enabled + NXP i.MX driver + kernel 5.4–5.14
}

/// Resolve which mmc_host a block device belongs to, then read CQHCI state.
/// device: e.g. /dev/mmcblk0
pub fn detect_cqhci(device: &Path) -> CqhciStatus;
```

**Resolving mmc_host from a block device**:

```
/sys/class/block/mmcblk0/device         → symlink target contains mmc_host name
or scan /sys/class/mmc_host/mmc*/       → find the one that contains mmcblk{N}
```

**sysfs reads inside `detect_cqhci()`**:

```
/sys/class/mmc_host/{host}/cmdq_enabled           → "0" or "1"
/sys/class/mmc_host/{host}/device/driver          → symlink basename = driver name
/sys/bus/platform/drivers/sdhci-esdhc-imx/        → exists = NXP i.MX driver active
```

**Risk classification**:

```rust
fn classify_risk(driver: &Option<String>, kernel: &str, enabled: bool) -> CqhciRisk {
    if !enabled { return CqhciRisk::None; }
    let is_nxp = driver.as_deref()
        .map(|d| d.contains("esdhc") || d.contains("usdhc"))
        .unwrap_or(false);
    if !is_nxp { return CqhciRisk::Low; }
    // Kernel version: "5.4.x" → major=5 minor=4
    let risky_kernel = parse_kernel_minor(kernel).map(|(maj, min)| maj == 5 && min < 15)
        .unwrap_or(false);
    if risky_kernel { CqhciRisk::High } else { CqhciRisk::Low }
}
```

### 5.3 Integration

- Add `cqhci: Option<CqhciStatus>` to `CapabilityReport` (already in `system.rs`).
- In `app.rs`, before starting any workload run, call `detect_cqhci()` on the primary
  device. If `risk == CqhciRisk::High`, print a clearly visible warning banner to
  the terminal (use existing `ui.rs` warning rendering). Do not block the run —
  just warn.
- Add to `doctor()` output as a named check with pass/warn/fail status.

---

## Task 6 — MMC Host State Snapshot (`system.rs`)

### 6.1 Struct and function

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MmcHostState {
    pub mmc_host: String,
    pub card_present: bool,
    pub card_type: Option<String>,    // "MMC"
    pub card_state: Option<String>,   // "active", "suspended", "disconnected"
    pub ios_clock_hz: Option<u64>,
    pub ios_bus_width: Option<u8>,
    pub ios_timing: Option<String>,   // "hs200", "hs400", "legacy", …
    pub ocr: Option<u32>,
    pub cmdq_enabled: Option<bool>,
    pub read_at: DateTime<Utc>,
}

pub fn read_mmc_host_state(device: &Path) -> Result<MmcHostState>;
```

**sysfs paths to read** (all optional — use `Option`, never fail if absent):

```
/sys/class/mmc_host/{host}/ios                    → multi-line key:value text
/sys/class/block/mmcblk{N}/device/state           → card state string
/sys/class/block/mmcblk{N}/device/type            → "MMC"
/sys/class/mmc_host/{host}/device/ocr             → hex string, kernel-dependent
```

**Parsing `ios`**: the file contains lines like `clock:\t\t200000000 Hz`,
`bus width:\t3 (8 bits)`, `timing spec:\tHS200`. Extract clock, bus_width,
and timing by key prefix match.

**OCR decoding** (display helper, not a probe):

```rust
pub fn decode_ocr(ocr: u32) -> String {
    let ready = (ocr >> 31) & 1 == 1;
    let hcs   = (ocr >> 30) & 1 == 1;
    format!("0x{:08X}  ready={}  hcs={}", ocr, ready, hcs)
}
```

### 6.2 Integration

- Add `mmc_host_state: Option<MmcHostState>` to `HealthReport`.
- Call `read_mmc_host_state()` inside `collect_health()` in `health.rs`.
- Render in `render_health_report()` in `ui.rs` under an "MMC Host" section.
- Add to `doctor()` — report card state and IOS clock as a check.

---

## Checklist

Work through the tasks in this exact order. Each task is independently compilable.

- [ ] **Task 1** `boot_partition.rs` — new file, lib.rs declaration, engine.rs integration,
      doctor check, unit test
- [ ] **Task 2** `integrity.rs` — new file, lib.rs declaration, CLI subcommand,
      SessionRecord additions (`#[serde(default)]`), auto-capture from profile,
      unit tests
- [ ] **Task 3** Checkpointing — `TestCheckpoint` in storage.rs, atomic save/load,
      profile `CheckpointConfig`, engine tick integration
- [ ] **Task 5** CQHCI — EXT_CSD bytes 307–308, `CqhciStatus`, `detect_cqhci()`,
      CapabilityReport, doctor, pre-run warning banner
- [ ] **Task 6** MMC host state — `MmcHostState`, `read_mmc_host_state()`,
      HealthReport, doctor, ui render
- [ ] **Task 4** Concurrent engine — `SecondaryTarget` in profile, worker pools in
      engine, `SecondaryRunSummary`, example profile YAML in `examples/profiles/`

## Coding conventions (from existing codebase)

- All new structs: `#[derive(Debug, Clone, Serialize, Deserialize)]`
- All new `SessionRecord` / `HealthReport` fields: `#[serde(default)]`
- sysfs reads: `std::fs::read_to_string(path).ok()?.trim().parse().ok()` — never panic
- No new async runtime dependencies
- No new TUI crate dependencies — UI is hand-rendered in `ui.rs`
- Unit tests in `#[cfg(test)]` inline modules with hardcoded fixture data
- Hardware-dependent tests: `#[test] #[ignore]` with a comment explaining requirements
