# emmc-lab: Storage Subsystem Expansion — Architecture & Agent Instructions

> **Audience**: AI coding agent with Rust expertise, tasked with implementing new subsystems in `emmc-lab`.
> **Codebase version**: 0.2.8 — 13k lines of Rust across 16 modules.
> **License**: MIT. **Target**: Linux only (eMMC and file-backed block devices).

---

## 1. Project Context

emmc-lab is a single-binary Rust CLI tool for Linux that tests, diagnoses, and monitors eMMC and file-backed storage. It combines:

- A workload engine (`engine.rs`) that executes precise I/O via `pread`/`pwrite` or `io_uring`
- Live procfs-based diagnostics (`diagnostics.rs`)
- eMMC health snapshot collection (`health.rs`) with EXT_CSD/CID/CSD parsing
- File-to-block extent mapping (`filemap.rs`) via FIEMAP/FIBMAP
- Block-layer tracing (`lba_trace.rs`) via blktrace/blkparse
- Multi-format reporting (`report.rs`) — JSON, CSV, HTML
- An interactive terminal UI (`ui.rs`) designed for SSH sessions
- Session persistence (`storage.rs`) and system capability detection (`system.rs`)

The architecture follows these principles:

1. **Single binary** — no runtime dependencies beyond standard Linux tools
2. **Graceful degradation** — every advanced feature has a fallback or clear "not available" path
3. **Safety first** — destructive operations require explicit flags and confirmation
4. **Session-based** — all data persisted as JSON/JSONL for offline analysis
5. **Honest about limitations** — never claims physical NAND visibility; all assertions are host-visible logical layer

---

## 2. What This Document Covers

Six new subsystems to bring emmc-lab to full logical/block/MMC/trace coverage:

| # | Subsystem | Primary Linux Interfaces |
|---|-----------|--------------------------|
| 1 | Block-device geometry & sector metadata | `/sys/class/block/*/queue/*`, sysfs attrs |
| 2 | TRIM / discard support | `BLKDISCARD`, `FITRIM`, `/sys/block/*/queue/discard_*` |
| 3 | File-to-block mapping enhancements | FIEMAP (already exists — extend it) |
| 4 | Richer block-level stats | `/proc/diskstats`, `/sys/block/*/stat` |
| 5 | MMC-specific low-level telemetry | `MMC_IOC_CMD`, sysfs CID/CSD, EXT_CSD (extend `health.rs`) |
| 6 | Low-level tracing + read-disturb modeling | blktrace, eBPF, endurance modeling |

---

## 3. Current Codebase Map (What Already Exists)

### 3.1 Module inventory

| File | Lines | Role |
|------|-------|------|
| `main.rs` | 5 | Entry point, calls `cli::run()` |
| `lib.rs` | 14 | Module declarations |
| `cli.rs` | 210 | Clap-derived CLI with subcommands |
| `app.rs` | 3616 | Interactive menu orchestration, wizard, all `run_*` flows |
| `engine.rs` | 895 | I/O workload execution (io_uring + pread/pwrite) |
| `profile.rs` | 547 | YAML workload profile model and validation |
| `presets.rs` | 436 | 13 built-in preset workload templates |
| `system.rs` | 796 | System snapshot, device discovery, capability checks |
| `diagnostics.rs` | 1147 | Procfs I/O sampling, deep-trace mode |
| `health.rs` | 861 | EXT_CSD parsing, CID/CSD, vendor-specific health |
| `health_compare.rs` | 356 | Compare live health vs failure-analysis reports |
| `filemap.rs` | 544 | FIEMAP/FIBMAP file extent mapping |
| `lba_trace.rs` | 364 | blktrace/blkparse integration |
| `report.rs` | 358 | JSON/CSV/HTML export |
| `storage.rs` | 198 | Session persistence (JSON/JSONL) |
| `ui.rs` | 2731 | Terminal UI, selectors, rendering |

### 3.2 Key types you will interact with

```rust
// system.rs — device metadata (you will EXTEND this)
pub struct DeviceInfo {
    pub path: PathBuf,
    pub name: String,
    pub parent: Option<String>,
    pub is_partition: bool,
    pub media_type: String,   // "disk", "part"
    pub model: String,
    pub size_bytes: u64,
    pub removable: bool,
    pub mounted: bool,
    pub mountpoints: Vec<PathBuf>,
    pub filesystem_types: Vec<String>,
    pub mount_options: Vec<String>,
    pub is_root_device: bool,
    pub logical_block_size: Option<u64>,
}

// health.rs — eMMC health (you will EXTEND this)
pub struct EmmcHealthSnapshot {
    pub device: PathBuf,
    pub device_life_time_est_typ_a: Option<u8>,
    pub device_life_time_est_typ_b: Option<u8>,
    pub pre_eol_info: Option<u8>,
    pub ext_csd_raw: Option<[u8; 512]>,
    pub ext_csd_rev: Option<u8>,
    pub bkops_support: Option<bool>,
    pub bkops_status: Option<u8>,
    pub bkops_urgency: Option<u8>,
    pub cache_size_kb: Option<u64>,
    pub cache_ctrl: Option<bool>,
    pub erase_group_size_bytes: Option<u64>,
    pub firmware_version: Option<String>,
    pub vendor_info: Option<VendorSpecificInfo>,
    pub cid: Option<CidInfo>,
    // ... more fields
}

// diagnostics.rs — device stats (you will EXTEND this)
pub struct DeviceSummary {
    // Currently: reads_completed, writes_completed, sectors_read, sectors_written,
    // time_spent_reading_ms, time_spent_writing_ms, ios_in_progress
}

// filemap.rs — extent mapping (you will EXTEND this)
pub struct FilemapReport {
    pub file_path: PathBuf,
    pub device: PathBuf,
    pub sector_size: u64,
    pub partition_start_lba: Option<u64>,
    pub erase_block_size: Option<u64>,
    pub extents: Vec<FilemapExtent>,
    pub total_extent_bytes: u64,
    pub extent_count: usize,
    pub method: String,
}
```

### 3.3 Dependencies (Cargo.toml)

```toml
anyhow = "1.0"
chrono = { version = "0.4", features = ["serde"] }
clap = { version = "4.5", features = ["derive"] }
crc32fast = "1.4"
libc = "0.2"
rand = "0.8"
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
serde_yaml = "0.9"
uuid = { version = "1.10", features = ["v4", "serde"] }
```

You will use `libc` for ioctl calls and `nix` (add as dependency) for safer syscall wrappers where appropriate.

### 3.4 Existing Linux interface usage

| Interface | Where used | Status |
|-----------|-----------|--------|
| `/sys/class/block/*/size` | `system.rs` `block_device_size()` | ✅ exists |
| `/sys/class/block/*/queue/logical_block_size` | `system.rs` `logical_block_size()` | ✅ exists |
| `/sys/class/block/*/queue/physical_block_size` | nowhere | ❌ NOT YET |
| `/sys/class/block/*/queue/minimum_io_size` | nowhere | ❌ NOT YET |
| `/sys/class/block/*/queue/optimal_io_size` | nowhere | ❌ NOT YET |
| `/sys/class/block/*/queue/discard_granularity` | nowhere | ❌ NOT YET |
| `/sys/class/block/*/queue/discard_max_bytes` | nowhere | ❌ NOT YET |
| `/sys/class/block/*/alignment_offset` | nowhere | ❌ NOT YET |
| `/sys/class/block/*/start` | `filemap.rs` (partition start) | ✅ exists |
| `/sys/class/block/*/ro` | nowhere | ❌ NOT YET |
| `/sys/class/block/*/removable` | `system.rs` | ✅ exists |
| `/proc/diskstats` | `system.rs` `diskstats_map()`, `diagnostics.rs` | ✅ basic parsing |
| FIEMAP ioctl | `filemap.rs` | ✅ exists |
| FIBMAP ioctl | `filemap.rs` (fallback) | ✅ exists |
| blktrace/blkparse | `lba_trace.rs` | ✅ CLI-based |
| eBPF/tracing detection | `system.rs` `deep_trace_available()` | ✅ detection only |
| EXT_CSD read | `health.rs` via debugfs ioctl | ✅ exists |
| CID/CSD from sysfs | `health.rs` | ✅ exists |
| MMC_IOC_CMD | nowhere | ❌ NOT YET |
| BLKDISCARD ioctl | nowhere | ❌ NOT YET |
| FITRIM ioctl | nowhere | ❌ NOT YET |
| BLKGETSIZE64 ioctl | nowhere (using sysfs instead) | ❌ NOT YET |
| BLKSSZGET ioctl | nowhere (using sysfs instead) | ❌ NOT YET |
| `/sys/block/*/stat` | nowhere | ❌ NOT YET |

---

## 4. Architecture Layers

emmc-lab organizes storage visibility into four layers. Every new feature must fit cleanly into one:

```
┌─────────────────────────────────────────────────┐
│  Layer 4: TRACE                                 │
│  Process/file/block activity, eBPF, blktrace    │
│  Files: diagnostics.rs, lba_trace.rs            │
├─────────────────────────────────────────────────┤
│  Layer 3: MMC                                   │
│  EXT_CSD, CID, CSD, health, vendor telemetry   │
│  Files: health.rs, health_compare.rs            │
├─────────────────────────────────────────────────┤
│  Layer 2: BLOCK                                 │
│  Sectors, partitions, queue geometry, discard    │
│  Files: system.rs (device info), NEW geometry.rs│
├─────────────────────────────────────────────────┤
│  Layer 1: FILESYSTEM                            │
│  Files, extents, trims, mounts                  │
│  Files: filemap.rs, engine.rs                   │
└─────────────────────────────────────────────────┘
```

---

## 5. Subsystem Specifications

### 5.1 Block-Device Geometry & Sector Metadata

**Goal**: Extend `DeviceInfo` with comprehensive block geometry so every other subsystem can use it.

**New file**: `geometry.rs` (or extend `system.rs` — your call, but a dedicated module is cleaner)

**Data to collect** (all from sysfs, no root required):

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockGeometry {
    pub device: PathBuf,

    // Sector sizes
    pub logical_block_size: u64,      // /sys/class/block/{dev}/queue/logical_block_size
    pub physical_block_size: u64,     // /sys/class/block/{dev}/queue/physical_block_size

    // I/O geometry
    pub minimum_io_size: u64,         // /sys/class/block/{dev}/queue/minimum_io_size
    pub optimal_io_size: u64,         // /sys/class/block/{dev}/queue/optimal_io_size

    // Discard geometry
    pub discard_granularity: u64,     // /sys/class/block/{dev}/queue/discard_granularity
    pub discard_max_bytes: u64,       // /sys/class/block/{dev}/queue/discard_max_bytes
    pub discard_zeroes_data: bool,    // /sys/class/block/{dev}/queue/discard_zeroes_data (may not exist on newer kernels)

    // Partition & alignment
    pub partition_start_lba: Option<u64>,  // /sys/class/block/{dev}/start  (only for partitions)
    pub alignment_offset: u64,             // /sys/class/block/{dev}/alignment_offset

    // Device flags
    pub read_only: bool,              // /sys/class/block/{dev}/ro
    pub removable: bool,              // /sys/class/block/{dev}/removable  (already in DeviceInfo)
    pub write_cache: Option<String>,  // /sys/class/block/{dev}/queue/write_cache ("write back" / "write through" / "none")

    // Capacity
    pub size_sectors: u64,            // /sys/class/block/{dev}/size
    pub size_bytes: u64,              // derived: size_sectors * logical_block_size

    // Scheduler
    pub scheduler: Option<String>,    // /sys/class/block/{dev}/queue/scheduler  (active scheduler in brackets)
}
```

**Implementation notes**:

1. Read each sysfs attribute with `fs::read_to_string()` + `.trim().parse()`. Every field can fail — use `Option` or `0` defaults with a note.
2. For partitions, the parent device name is in `/sys/class/block/{part}/..` or by stripping trailing digits. The existing `DeviceInfo.parent` field already handles this.
3. `write_cache` is a string like `"write back [write through]"` — parse the active one (in brackets or the only value).
4. `scheduler` looks like `"none [mq-deadline] kyber"` — extract the bracketed one.

**Integration points**:

- Add `pub geometry: Option<BlockGeometry>` to `DeviceInfo` (or collect separately)
- Call `collect_geometry(device_path)` from `list_devices()` and the doctor command
- Surface in `render_device_list()` in `ui.rs`
- Add a new CLI subcommand: `emmc-lab geometry --device /dev/mmcblk0`
- Include in session records: `SessionRecord.geometry: Option<BlockGeometry>`
- The engine should warn if `block_size_bytes` in a profile doesn't align with `physical_block_size`
- The engine should warn if addressing range isn't aligned to `optimal_io_size`

**Sysfs paths to read** (for a device named e.g. `mmcblk0` or `mmcblk0p1`):

```
/sys/class/block/{name}/queue/logical_block_size
/sys/class/block/{name}/queue/physical_block_size
/sys/class/block/{name}/queue/minimum_io_size
/sys/class/block/{name}/queue/optimal_io_size
/sys/class/block/{name}/queue/discard_granularity
/sys/class/block/{name}/queue/discard_max_bytes
/sys/class/block/{name}/queue/discard_zeroes_data
/sys/class/block/{name}/queue/write_cache
/sys/class/block/{name}/queue/scheduler
/sys/class/block/{name}/start
/sys/class/block/{name}/alignment_offset
/sys/class/block/{name}/ro
/sys/class/block/{name}/removable
/sys/class/block/{name}/size
```

For partitions, queue attributes live on the parent device. Detect this: if `/sys/class/block/{name}/queue/` doesn't exist or is empty, resolve the parent (e.g., `mmcblk0p1` → `mmcblk0`) and read from there.

---

### 5.2 TRIM / Discard Support

**Goal**: Detect, probe, and optionally exercise TRIM/discard on test regions.

**New file**: `discard.rs`

**Capabilities to implement**:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscardCapability {
    pub device: PathBuf,

    // Detection
    pub discard_supported: bool,       // discard_granularity > 0
    pub discard_granularity: u64,      // from geometry
    pub discard_max_bytes: u64,        // from geometry

    // Availability
    pub blkdiscard_available: bool,    // command_exists("blkdiscard")
    pub fstrim_available: bool,        // command_exists("fstrim")
    pub blkdiscard_ioctl_viable: bool, // can we open device + BLKDISCARD?

    // Probe results (if safe probe was run)
    pub probe_result: Option<DiscardProbeResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscardProbeResult {
    pub probe_offset: u64,
    pub probe_length: u64,
    pub success: bool,
    pub duration_us: u64,
    pub error: Option<String>,
}
```

**Implementation**:

1. **Detection**: Entirely from `BlockGeometry` — `discard_granularity > 0` means the device advertises discard support. No ioctl needed for detection.

2. **Tool availability**:
   ```rust
   pub fn detect_discard(device: &Path, geometry: &BlockGeometry) -> DiscardCapability
   ```
   Check `command_exists("blkdiscard")` and `command_exists("fstrim")`. Both are in `util-linux`.

3. **Safe discard probe** (requires explicit opt-in, unmounted device, confirmation):
   ```rust
   pub fn probe_discard(device: &Path, offset: u64, length: u64) -> Result<DiscardProbeResult>
   ```
   Uses the `BLKDISCARD` ioctl:
   ```rust
   // ioctl number: _IO(0x12, 119)  or use libc::BLKDISCARD if available
   // Argument: [u64; 2] = [offset, length]
   const BLKDISCARD: libc::c_ulong = 0x1277; // _IO(0x12, 119)

   let range: [u64; 2] = [offset, length];
   unsafe { libc::ioctl(fd, BLKDISCARD, &range) };
   ```

   **Safety requirements** — enforce ALL of these before issuing BLKDISCARD:
   - Device must NOT be mounted (check `/proc/self/mountinfo`)
   - Device must NOT be a root device
   - Must NOT be the whole-disk device if partitions exist and are mounted
   - Caller must pass an explicit `--i-understand-this-will-discard-data` flag
   - Offset and length must be aligned to `discard_granularity`
   - Length must not exceed `discard_max_bytes`

4. **Controlled discard test** (advanced — for test regions only):
   ```rust
   pub fn run_discard_test(device: &Path, ranges: &[(u64, u64)]) -> Result<Vec<DiscardProbeResult>>
   ```
   Issues multiple discards across a region and measures latency per discard. This is destructive.

5. **FITRIM support** (filesystem-level):
   ```rust
   // ioctl FITRIM on a mounted filesystem directory fd
   // struct fstrim_range { start: u64, len: u64, minlen: u64 }
   pub fn check_fstrim(mountpoint: &Path) -> Result<bool>
   ```
   Only detect availability — don't actually issue FITRIM without explicit user request.

**CLI integration**:

```
emmc-lab discard --device /dev/mmcblk0
    → prints DiscardCapability (detection only, safe)

emmc-lab discard --device /dev/mmcblk0 --probe --offset 0 --length 4194304
    → runs safe probe on 4MB region (requires unmounted + confirmation flag)

emmc-lab discard --device /dev/mmcblk0 --test --offset 0 --length 67108864 --i-understand-this-will-discard-data
    → controlled discard test across 64MB (destructive)
```

**Integration with other subsystems**:

- Include `DiscardCapability` in `HealthReport` and `SessionRecord`
- The doctor command should check and report discard capability
- The geometry display should show discard fields prominently
- LBA trace events already capture `IoDirection::Discard` — correlate with discard test results

---

### 5.3 File-to-Block Mapping Enhancements

**Goal**: Extend the existing `filemap.rs` to provide richer extent information and better device attribution.

**What already exists**: FIEMAP and FIBMAP ioctl wrappers, extent collection, partition offset detection, CSV export.

**What to add**:

1. **Backing device resolution**: Currently `filemap.rs` resolves the mount device from `/proc/self/mountinfo`. Enhance to:
   - Resolve through device-mapper (LVM, dm-crypt) if present: read `/sys/class/block/{dm_name}/slaves/`
   - Resolve through md-raid if present
   - Show the full device chain: `file → /dev/mapper/vg-lv → /dev/mmcblk0p2 → /dev/mmcblk0`

2. **Extent range summary**: Add to `FilemapReport`:
   ```rust
   pub struct ExtentSummary {
       pub total_extents: usize,
       pub total_bytes: u64,
       pub fragmentation_ratio: f64,   // extents / ideal_extents (1.0 = perfect)
       pub physical_range_min: u64,    // lowest physical offset
       pub physical_range_max: u64,    // highest physical offset + length
       pub physical_span_bytes: u64,   // max - min (how spread out)
       pub gaps: Vec<(u64, u64)>,      // physical gaps between extents (offset, size)
       pub shared_extents: usize,      // extents with FIEMAP_EXTENT_SHARED flag
       pub unwritten_extents: usize,   // extents with FIEMAP_EXTENT_UNWRITTEN flag
       pub inline_extents: usize,      // extents with FIEMAP_EXTENT_DATA_INLINE flag
   }
   ```

3. **Multi-file mapping**: Accept a list of files or a directory and produce a combined report showing how files are interleaved on disk:
   ```rust
   pub fn collect_multi_file_map(files: &[PathBuf]) -> Result<Vec<FilemapReport>>
   ```

4. **Erase-block alignment analysis**: Given the erase block size (from `health.rs` or geometry), report how many extents cross erase-block boundaries (which causes write amplification):
   ```rust
   pub struct EraseBlockAlignment {
       pub erase_block_size: u64,
       pub extents_aligned: usize,
       pub extents_crossing: usize,
       pub crossing_details: Vec<(u64, u64, usize)>, // (offset, length, blocks_spanned)
   }
   ```

**CLI changes**: Extend the existing `filemap` subcommand:

```
emmc-lab filemap <FILE> [--device DEV] [--csv] [--summary] [--erase-align]
emmc-lab filemap --dir <DIRECTORY> [--device DEV]  # multi-file mode
```

---

### 5.4 Richer Block-Level Stats

**Goal**: Parse `/proc/diskstats` and `/sys/block/*/stat` more thoroughly to derive throughput, queue depth, await-like latencies, and merge ratios.

**Extend**: `diagnostics.rs` `DeviceSummary` struct.

**Current `/proc/diskstats` fields** (kernel 4.18+, 20 fields per device):

```
Field  1: reads completed
Field  2: reads merged
Field  3: sectors read
Field  4: time spent reading (ms)
Field  5: writes completed
Field  6: writes merged
Field  7: sectors written
Field  8: time spent writing (ms)
Field  9: I/Os currently in progress
Field 10: time spent doing I/Os (ms)
Field 11: weighted time spent doing I/Os (ms)
Field 12: discards completed (4.18+)
Field 13: discards merged (4.18+)
Field 14: sectors discarded (4.18+)
Field 15: time spent discarding (ms) (4.18+)
Field 16: flush requests completed (5.5+)
Field 17: time spent flushing (ms) (5.5+)
```

**New struct**:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockStats {
    pub device: String,
    pub timestamp: DateTime<Utc>,

    // Raw counters (from /proc/diskstats or /sys/block/*/stat)
    pub reads_completed: u64,
    pub reads_merged: u64,
    pub sectors_read: u64,
    pub read_time_ms: u64,

    pub writes_completed: u64,
    pub writes_merged: u64,
    pub sectors_written: u64,
    pub write_time_ms: u64,

    pub ios_in_progress: u64,
    pub io_time_ms: u64,
    pub weighted_io_time_ms: u64,

    pub discards_completed: Option<u64>,
    pub discards_merged: Option<u64>,
    pub sectors_discarded: Option<u64>,
    pub discard_time_ms: Option<u64>,

    pub flush_requests: Option<u64>,
    pub flush_time_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockStatsDelta {
    pub device: String,
    pub interval_ms: u64,

    // Derived rates
    pub read_iops: f64,
    pub write_iops: f64,
    pub discard_iops: Option<f64>,
    pub flush_rate: Option<f64>,

    pub read_throughput_bytes_per_sec: f64,
    pub write_throughput_bytes_per_sec: f64,

    // Merge ratios (higher = more merging = better)
    pub read_merge_ratio: f64,   // reads_merged / (reads_completed + reads_merged)
    pub write_merge_ratio: f64,

    // Latency estimates (from time_ms / completed)
    pub avg_read_await_ms: f64,
    pub avg_write_await_ms: f64,
    pub avg_discard_await_ms: Option<f64>,
    pub avg_flush_await_ms: Option<f64>,

    // Queue depth estimate
    pub avg_queue_depth: f64,     // weighted_io_time_ms / interval_ms
    pub utilization_percent: f64, // io_time_ms / interval_ms * 100

    // Raw delta (for inspection)
    pub raw: BlockStats,
}
```

**Implementation**:

1. **Snapshot function**:
   ```rust
   pub fn read_block_stats(device: &str) -> Result<BlockStats>
   ```
   Parse `/proc/diskstats` line for the given device. Handle both old (11-field) and new (17-field) formats gracefully.

2. **Delta computation**:
   ```rust
   pub fn compute_block_stats_delta(prev: &BlockStats, curr: &BlockStats, interval_ms: u64) -> BlockStatsDelta
   ```
   Subtract counters, compute rates. Guard against division by zero and counter wraps.

3. **Continuous sampling**: Integrate with `LiveSamplerState` in `diagnostics.rs`:
   - Take a `BlockStats` snapshot each tick
   - Compute deltas
   - Store in `DiagnosticReport`

4. **Add to DiagnosticReport**:
   ```rust
   pub struct DiagnosticReport {
       // ... existing fields ...
       pub block_stats_timeline: Vec<BlockStatsDelta>,  // NEW
   }
   ```

**Key derivation formulas**:

```
read_iops = delta_reads_completed / (interval_ms / 1000)
read_throughput = (delta_sectors_read * sector_size) / (interval_ms / 1000)
read_merge_ratio = delta_reads_merged / (delta_reads_completed + delta_reads_merged)
avg_read_await = delta_read_time_ms / delta_reads_completed  (if reads > 0)
avg_queue_depth = delta_weighted_io_time_ms / interval_ms
utilization = (delta_io_time_ms / interval_ms) * 100  (capped at 100%)
```

**CLI**: No new subcommand needed — enrich the existing `diag monitor` and `diag sample` output. Add a `--device` filter flag if not present.

---

### 5.5 MMC-Specific Low-Level Telemetry

**Goal**: Extend `health.rs` with comprehensive MMC register decoding and direct `MMC_IOC_CMD` support.

**What already exists**: EXT_CSD raw read (via debugfs), partial field parsing, CID/CSD from sysfs, vendor-specific health data (Samsung, SanDisk, Micron, Hynix).

**What to add**:

#### 5.5.1 Full EXT_CSD Decoding

The EXT_CSD register is 512 bytes. The current code parses ~15 fields. Expand to include all operationally relevant fields:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtCsdDecoded {
    // Existing fields (already parsed — keep them)
    pub ext_csd_rev: u8,                    // [192]
    pub device_life_time_est_typ_a: u8,     // [268]
    pub device_life_time_est_typ_b: u8,     // [269]
    pub pre_eol_info: u8,                   // [267]
    pub sec_count: u32,                     // [215:212]
    pub firmware_version: String,           // [261:254]

    // NEW fields to parse
    // Boot configuration
    pub boot_size_mult: u8,                 // [226] — boot partition size = 128KB * boot_size_mult
    pub boot_info: u8,                      // [228] — boot partition features
    pub partition_config: u8,               // [179] — boot partition and access config
    pub boot_bus_conditions: u8,            // [177]
    pub rst_n_function: u8,                 // [162] — HW reset function

    // RPMB
    pub rpmb_size_mult: u8,                // [168] — RPMB partition size = 128KB * rpmb_size_mult

    // Partitioning
    pub partitioning_support: u8,          // [160]
    pub max_enh_size_mult: u32,            // [159:157]
    pub partitions_attribute: u8,          // [156]
    pub partition_setting_completed: u8,   // [155]
    pub gp_size_mult: [[u8; 3]; 4],       // [154:143] — 4 GP partitions × 3 bytes each

    // Cache & BKOPS (extend existing)
    pub cache_size: u32,                   // [252:249] — 4 bytes, KB
    pub cache_ctrl: u8,                    // [33]
    pub cache_flushed: bool,              // [32] — power notification cache flushed
    pub bkops_en: u8,                     // [163]
    pub bkops_start: u8,                  // [164]
    pub bkops_support: u8,               // [502]
    pub bkops_status: u8,                // [246]
    pub hpi_features: u8,                // [503]
    pub hpi_mgmt: u8,                    // [161]

    // Erase
    pub erase_group_def: u8,             // [175]
    pub hc_erase_grp_size: u8,           // [224] — erase unit = 512KB * hc_erase_grp_size
    pub hc_wp_grp_size: u8,              // [221] — write protect group size

    // Reliable write
    pub rel_wr_sec_c: u8,               // [222] — reliable write sector count
    pub wr_rel_set: u8,                 // [167] — write reliability setting register
    pub wr_rel_param: u8,              // [166] — write reliability parameter register

    // Device type and speed
    pub device_type: u8,                // [196]
    pub hs_timing: u8,                  // [185]
    pub bus_width: u8,                  // [183]
    pub strobe_support: u8,             // [184]

    // Power management
    pub power_class: u8,               // various registers based on speed mode
    pub power_off_notification: u8,    // [34]
    pub power_off_long_time: u8,       // [247] — ms
    pub sleep_notification_time: u8,   // [216]

    // Command queue
    pub cmdq_support: u8,             // [308]
    pub cmdq_depth: u8,               // [307]

    // Secure features
    pub secure_removal_type: u8,      // [16]
    pub secure_feature_support: u8,   // [231]
    pub sec_trim_mult: u8,            // [229]
    pub sec_erase_mult: u8,           // [230]

    // FFU (Field Firmware Update)
    pub ffu_features: u8,             // [492]
    pub supported_modes: u8,          // [493]
    pub ffu_arg: u32,                 // [490:487]
    pub number_of_fw_sectors_correctly_programmed: u32,  // [305:302]
}
```

**Decoding function**:

```rust
pub fn decode_ext_csd(raw: &[u8; 512]) -> ExtCsdDecoded {
    // Each field is at a known byte offset in the 512-byte register
    // Multi-byte fields are little-endian
    ExtCsdDecoded {
        ext_csd_rev: raw[192],
        sec_count: u32::from_le_bytes([raw[212], raw[213], raw[214], raw[215]]),
        boot_size_mult: raw[226],
        rpmb_size_mult: raw[168],
        partition_config: raw[179],
        // ... etc
    }
}
```

#### 5.5.2 CID Register Full Decoding

CID is already partially parsed. Extend `CidInfo`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CidInfo {
    // Existing
    pub mid: u8,           // Manufacturer ID
    pub oid: String,       // OEM/Application ID (2 chars)
    pub pnm: String,       // Product Name (6 chars for eMMC)
    pub prv: String,       // Product Revision (major.minor)
    pub psn: u32,          // Product Serial Number
    pub mdt: String,       // Manufacturing Date (month/year)
    pub raw_hex: String,

    // NEW
    pub cbx: u8,           // Card/BGA type (0=card, 1=BGA)
    pub prv_major: u8,
    pub prv_minor: u8,
    pub mdt_year: u16,
    pub mdt_month: u8,
    pub crc: u8,
}
```

#### 5.5.3 CSD Register Decoding

CSD is NOT currently parsed (only CID is). Add:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CsdInfo {
    pub csd_structure: u8,        // CSD version (should be 3 for eMMC ≥4.1)
    pub spec_vers: u8,            // System specification version
    pub taac: u8,                 // Data read access time
    pub nsac: u8,                 // Data read access time in CLK cycles
    pub tran_speed: u8,           // Max bus clock frequency
    pub ccc: u16,                 // Card command classes
    pub read_bl_len: u8,          // Max read data block length
    pub read_bl_partial: bool,
    pub write_blk_misalign: bool,
    pub read_blk_misalign: bool,
    pub dsr_imp: bool,            // DSR implemented
    pub c_size: u32,              // Device size
    pub vdd_r_curr_min: u8,
    pub vdd_r_curr_max: u8,
    pub vdd_w_curr_min: u8,
    pub vdd_w_curr_max: u8,
    pub c_size_mult: u8,
    pub erase_grp_size: u8,
    pub erase_grp_mult: u8,
    pub wp_grp_size: u8,
    pub wp_grp_enable: bool,
    pub r2w_factor: u8,
    pub write_bl_len: u8,
    pub write_bl_partial: bool,
    pub content_prot_app: bool,
    pub file_format_grp: bool,
    pub copy: bool,
    pub perm_write_protect: bool,
    pub tmp_write_protect: bool,
    pub file_format: u8,
    pub raw_hex: String,
}
```

Read from sysfs: `/sys/class/block/mmcblk{N}/device/csd`

#### 5.5.4 Direct MMC_IOC_CMD (Optional Advanced)

For deeper register reads beyond what debugfs/sysfs expose. This is the advanced path — implement only after the sysfs-based parsing is complete.

```rust
use libc::{c_ulong, ioctl};

// From <linux/mmc/ioctl.h>
#[repr(C)]
struct MmcIocCmd {
    write_flag: i32,      // 0 for read, 1 for write
    opcode: u32,          // MMC command opcode
    arg: u32,             // Command argument
    response: [u32; 4],   // Response data
    flags: u32,           // Command flags
    blksz: u32,           // Block size
    blocks: u32,           // Number of blocks
    data_ptr: u64,        // Pointer to data buffer (cast from *mut u8)
    // ... padding fields
}

// ioctl number for MMC_IOC_CMD
const MMC_IOC_CMD: c_ulong = 0xC048B300; // _IOWR('B', 0, struct mmc_ioc_cmd)
```

**Use cases for MMC_IOC_CMD**:
- Send SEND_EXT_CSD (CMD8) directly instead of debugfs
- Send SEND_STATUS (CMD13) for real-time status
- Send SWITCH (CMD6) to query write-protect status without modifying

**Safety**: NEVER issue write/erase/switch commands through this interface. Read-only queries only. Wrap in a capability check and refuse on non-MMC devices.

#### 5.5.5 Partition Configuration Display

Derive from EXT_CSD fields:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MmcPartitionInfo {
    pub boot_partition_1_size_kb: u64,   // boot_size_mult * 128
    pub boot_partition_2_size_kb: u64,   // same
    pub rpmb_size_kb: u64,              // rpmb_size_mult * 128
    pub boot_partition_enabled: bool,    // from partition_config
    pub boot_ack_enabled: bool,          // from partition_config
    pub access_partition: u8,            // from partition_config (which partition is accessed)
    pub gp_partitions: Vec<GpPartitionInfo>,
    pub enhanced_area: Option<EnhancedAreaInfo>,
}
```

#### 5.5.6 Health Bucket Decoding

The `DEVICE_LIFE_TIME_EST_TYP_A` (byte [268]) and `TYP_B` (byte [269]) values are:

| Value | Meaning |
|-------|---------|
| 0x00 | Not defined |
| 0x01 | 0%–10% device life time used |
| 0x02 | 10%–20% |
| ... | ... |
| 0x0A | 90%–100% |
| 0x0B | Exceeded maximum |

`PRE_EOL_INFO` (byte [267]):

| Value | Meaning |
|-------|---------|
| 0x00 | Not defined |
| 0x01 | Normal |
| 0x02 | Warning (consumed 80% of reserved blocks) |
| 0x03 | Urgent |

Add a human-readable decoder:

```rust
pub fn decode_life_time_estimate(value: u8) -> String {
    match value {
        0x00 => "Not defined".to_string(),
        0x01..=0x0A => format!("{}%–{}% life used", (value - 1) * 10, value * 10),
        0x0B => "Exceeded maximum estimated life".to_string(),
        _ => format!("Reserved (0x{:02X})", value),
    }
}

pub fn decode_pre_eol(value: u8) -> &'static str {
    match value {
        0x00 => "Not defined",
        0x01 => "Normal",
        0x02 => "Warning (consumed 80% of reserved blocks)",
        0x03 => "Urgent (consumed 90% of reserved blocks)",
        _ => "Reserved",
    }
}
```

**Integration**: Update `render_health_report()` in `ui.rs` to show decoded health buckets, boot/RPMB sizes, partition config, cache state, BKOPS status with human-readable labels.

---

### 5.6 Low-Level Tracing & Read-Disturb Modeling

**Goal**: Strengthen tracing capabilities and refine the read-disturb endurance model.

#### 5.6.1 blktrace Enhancements

The current `lba_trace.rs` shells out to `blktrace`/`blkparse`. Enhance:

1. **Event categorization**: Group trace events into buckets:
   ```rust
   pub struct TraceEventSummary {
       pub total_events: u64,
       pub read_events: u64,
       pub write_events: u64,
       pub discard_events: u64,
       pub flush_events: u64,

       // LBA heatmap (divide device into N equal ranges, count events per range)
       pub lba_heatmap: Vec<LbaHeatmapBucket>,

       // Temporal patterns
       pub events_per_second: Vec<u64>,
       pub peak_iops_read: f64,
       pub peak_iops_write: f64,

       // Attribution
       pub attributed_events: u64,
       pub unattributed_events: u64,    // kernel I/O, journal, background
       pub kernel_io_estimate: u64,      // events with no userspace process
   }

   pub struct LbaHeatmapBucket {
       pub start_lba: u64,
       pub end_lba: u64,
       pub read_count: u64,
       pub write_count: u64,
       pub discard_count: u64,
   }
   ```

2. **Kernel/unattributed I/O detection**: blktrace events that come from `kworker`, `jbd2`, `ksoftirqd`, `mmcqd`, or have PID 0 should be tagged as kernel I/O. Report these separately:
   ```rust
   pub struct KernelIoBucket {
       pub source: String,        // "jbd2", "kworker", "mmcqd", "unknown-kernel"
       pub read_events: u64,
       pub write_events: u64,
       pub sectors_read: u64,
       pub sectors_written: u64,
   }
   ```

3. **blkparse format**: Use a machine-friendly output format for parsing:
   ```
   blkparse -f "%T.%t %p %a %d %S %n\n" -i trace_output
   ```
   Where: `%T.%t` = timestamp, `%p` = PID, `%a` = action, `%d` = direction, `%S` = sector, `%n` = bytes.

#### 5.6.2 eBPF Integration

Currently, `diagnostics.rs` has `run_deep_trace()` with eBPF detection. Expand:

1. **BCC/bpftrace probe** — if `bpftrace` is available, run a targeted script:
   ```
   bpftrace -e '
     tracepoint:block:block_rq_complete {
       printf("%llu %d %s %llu %u\n", nsecs, pid, comm, args->sector, args->nr_sector);
     }
   ' -c "sleep {duration}"
   ```

   Parse the output into `TraceEvent` structs.

2. **Syscall-level tracing** (optional, if bpftrace is available):
   ```
   tracepoint:syscalls:sys_enter_read
   tracepoint:syscalls:sys_enter_write
   tracepoint:syscalls:sys_enter_fsync
   tracepoint:syscalls:sys_enter_fdatasync
   ```
   Correlate PID + fd → file → device → LBA for full attribution chain.

3. **Graceful fallback chain**:
   ```
   eBPF (bpftrace) → blktrace/blkparse → procfs sampler
   ```
   Always try the best available, fall back transparently.

**Detection function**:
```rust
pub fn detect_trace_capabilities() -> TraceCapabilities {
    TraceCapabilities {
        bpftrace_available: command_exists("bpftrace"),
        blktrace_available: command_exists("blktrace") && command_exists("blkparse"),
        debugfs_mounted: Path::new("/sys/kernel/debug").exists(),
        tracefs_available: Path::new("/sys/kernel/tracing").exists()
            || Path::new("/sys/kernel/debug/tracing").exists(),
        is_root: unsafe { libc::geteuid() } == 0,
    }
}
```

#### 5.6.3 Read-Disturb / Endurance Modeling

`health.rs` already has `build_read_disturb_model()`. Enhance:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnduranceModel {
    // Device geometry (inputs)
    pub device_capacity_bytes: u64,
    pub erase_block_size_bytes: u64,
    pub page_size_bytes: u64,
    pub pages_per_block: u64,

    // Host-visible counters (inputs)
    pub total_sectors_written: u64,      // from diskstats lifetime or session
    pub total_sectors_read: u64,
    pub total_discards: u64,

    // Derived estimates
    pub total_blocks: u64,                         // capacity / erase_block_size
    pub host_write_amplification_estimate: f64,    // if session data available
    pub estimated_program_erase_cycles: f64,       // total_written / capacity (naive)
    pub reads_per_block_estimate: f64,             // total_read / total_blocks

    // Read-disturb assessment
    pub read_disturb_risk: ReadDisturbRisk,
    pub read_disturb_reads_per_block: f64,
    pub read_disturb_threshold: u64,              // user-configurable
    pub read_disturb_ratio: f64,                  // reads_per_block / threshold

    // Caveats — ALWAYS PRESENT
    pub caveats: Vec<String>,
    // Must include:
    // "All estimates are based on host-visible logical I/O, not physical NAND operations"
    // "FTL wear-leveling, garbage collection, and write amplification are not visible"
    // "Actual NAND wear may be 2-10x higher than host-visible writes suggest"
    // "Read-disturb thresholds vary by NAND technology (SLC/MLC/TLC) and are not readable"
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ReadDisturbRisk {
    Low,       // ratio < 0.3
    Moderate,  // ratio 0.3 - 0.7
    Elevated,  // ratio 0.7 - 1.0
    High,      // ratio > 1.0
    Unknown,   // insufficient data
}
```

**Critical honesty requirement**: Every endurance estimate MUST include caveats. We cannot see past the FTL. The model is useful for relative comparison and trend monitoring, not absolute truth.

---

## 6. Implementation Order

Implement in this order — each builds on the previous:

```
Phase 1: Foundation
  ├── 5.1 Block-device geometry (geometry.rs)        ← everything else needs this
  └── 5.4 Richer block stats (extend diagnostics.rs) ← needed for endurance model

Phase 2: Core features
  ├── 5.5 MMC telemetry (extend health.rs)           ← EXT_CSD/CID/CSD full decode
  └── 5.3 File mapping enhancements (extend filemap.rs)

Phase 3: Advanced
  ├── 5.2 TRIM/discard (discard.rs)                  ← needs geometry + safety
  └── 5.6 Tracing + endurance (extend lba_trace.rs, health.rs)
```

---

## 7. Cross-Cutting Concerns

### 7.1 Error Handling

Use `anyhow::Result` throughout. Every sysfs read can fail — use `Option<T>` for fields that may not be present. Never panic on missing kernel interfaces.

Pattern:
```rust
fn read_sysfs_u64(path: &Path) -> Option<u64> {
    std::fs::read_to_string(path)
        .ok()?
        .trim()
        .parse()
        .ok()
}
```

### 7.2 Graceful Degradation

Every feature must work in three modes:
1. **Full** — all prerequisites met (root, device accessible, kernel support)
2. **Partial** — some data available (non-root, some sysfs readable)
3. **Unavailable** — feature clearly marked as unavailable with reason

Never crash. Never lie. Always explain what's missing.

### 7.3 Serialization

All new structs must derive `Serialize, Deserialize` (serde). They will be included in session JSON exports.

### 7.4 CLI Integration

New subcommands should follow the existing pattern in `cli.rs`:
- Use clap derive macros
- Match in `run()` dispatcher
- Call into `app.rs` for orchestration
- `app.rs` calls the module functions and handles rendering via `ui.rs`

### 7.5 UI Integration

All new data should be renderable in the terminal. Follow the existing style in `ui.rs`:
- Use `render_*` functions that return `String`
- Respect terminal width (query with `term_width()`)
- Use the existing color/formatting helpers
- No external TUI crates — everything is hand-rendered

### 7.6 Testing

- Unit tests for all parsing functions (sysfs, EXT_CSD, diskstats)
- Use `#[cfg(test)]` modules with hardcoded sample data
- For ioctl-dependent code, provide mock paths or skip-if-not-available test logic
- Test all Option/error paths

### 7.7 Safety Rules

1. **Never write to a device without explicit destructive confirmation flag**
2. **Never issue BLKDISCARD without mount checks + confirmation**
3. **Never issue MMC_IOC_CMD with write_flag=1**
4. **Never target root/boot devices for destructive operations**
5. **Always check mount status before raw device operations**
6. **TRIM probe on mounted filesystems: detection only, never issue FITRIM without explicit opt-in**

---

## 8. What You Must NOT Claim

This section is part of the project's integrity contract. When writing user-facing output, documentation, or log messages:

**Do NOT claim**:
- Physical NAND page/block targeting capability
- Exact NAND wear per cell or per block
- Exact post-FTL data placement knowledge
- Exact per-file block attribution during delayed writeback
- Exact write amplification measurement (only estimates)
- Real-time NAND error rates (only EXT_CSD health buckets)

**DO say**:
- "Host-visible logical I/O statistics"
- "Estimated based on block-layer counters"
- "Inferred from EXT_CSD health registers, not direct NAND measurement"
- "FTL behavior is opaque — actual physical wear may differ significantly"

For eMMC, the honest ceiling is:
- **Excellent**: logical/block/MMC register visibility
- **Good**: statistical endurance modeling from host-visible data
- **Not possible**: true raw NAND visibility through the FTL

---

## 9. File Placement Summary

| What | Where | Action |
|------|-------|--------|
| Block geometry | `src/geometry.rs` (new) | Create |
| Discard/TRIM | `src/discard.rs` (new) | Create |
| Block stats | `src/diagnostics.rs` | Extend |
| MMC telemetry | `src/health.rs` | Extend |
| CSD parsing | `src/health.rs` | Extend |
| File mapping | `src/filemap.rs` | Extend |
| Tracing enhancements | `src/lba_trace.rs` | Extend |
| Endurance model | `src/health.rs` | Extend |
| CLI commands | `src/cli.rs` | Extend |
| Orchestration | `src/app.rs` | Extend |
| UI rendering | `src/ui.rs` | Extend |
| Module declarations | `src/lib.rs` | Extend |
| Cargo dependencies | `Cargo.toml` | Maybe add `nix` crate |

---

## 10. Dependency Notes

The project currently has minimal dependencies. Keep it that way.

- **Do NOT add**: tokio, async-std, or any async runtime (the tool is synchronous by design)
- **Do NOT add**: TUI crates (ratatui, crossterm, etc.) — the UI is hand-rendered
- **Consider adding**: `nix` crate for safer ioctl/syscall wrappers (optional — `libc` is fine too)
- **Consider adding**: `bitflags` crate if flag decoding becomes unwieldy
- All sysfs/procfs reading should use `std::fs` — no special crate needed

---

## 11. Testing Approach

For code that reads from `/sys` or `/proc`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_diskstats_line() {
        let line = "   8       0 sda 12345 678 901234 5678 9012 345 678901 2345 0 6789 12345";
        let stats = parse_diskstats_line(line).unwrap();
        assert_eq!(stats.reads_completed, 12345);
        assert_eq!(stats.reads_merged, 678);
        // ...
    }

    #[test]
    fn test_decode_ext_csd_boot_config() {
        let mut raw = [0u8; 512];
        raw[226] = 8;    // boot_size_mult = 8 → 1MB boot partitions
        raw[168] = 2;    // rpmb_size_mult = 2 → 256KB RPMB
        raw[179] = 0x48; // partition_config
        let decoded = decode_ext_csd(&raw);
        assert_eq!(decoded.boot_size_mult, 8);
        assert_eq!(decoded.rpmb_size_mult, 2);
    }

    #[test]
    fn test_discard_detection_no_support() {
        let geom = BlockGeometry {
            discard_granularity: 0,
            discard_max_bytes: 0,
            // ...
        };
        let cap = detect_discard_from_geometry(&geom);
        assert!(!cap.discard_supported);
    }
}
```

For ioctl-dependent code, use `#[ignore]` tests that only run on real hardware:

```rust
#[test]
#[ignore] // Run with: cargo test -- --ignored (requires real eMMC device)
fn test_real_blkdiscard_probe() {
    // Only runs when explicitly requested on real hardware
}
```

---

## 12. Output Format Integration

All new data structures must flow into the existing export pipeline:

1. **JSON**: Automatically handled by serde `Serialize` on `SessionRecord`
2. **CSV**: Add relevant fields to the CSV summary row in `report.rs`
3. **HTML**: Add sections to the HTML report template in `report.rs`
4. **Terminal**: Add `render_*` functions in `ui.rs`

For the geometry and MMC telemetry, the terminal output should look like:

```
──── Block Geometry: /dev/mmcblk0 ────
Logical block size    512 B
Physical block size   512 B
Minimum I/O           512 B
Optimal I/O           0 B (not specified)
Discard granularity   4,194,304 B (4 MiB)
Discard max           4,194,304 B (4 MiB)
Write cache           write back
Scheduler             [mq-deadline]
Alignment offset      0
Partition start       8192 sectors

──── eMMC Health: /dev/mmcblk0 ────
Life time Type A      0%–10% used
Life time Type B      0%–10% used
Pre-EOL info          Normal
Boot partition 1      4 MiB (boot_size_mult=32)
Boot partition 2      4 MiB
RPMB                  4 MiB (rpmb_size_mult=32)
Cache                 Enabled, 8 MiB
BKOPS                 Supported, status=idle
Erase group           4 MiB
Reliable write        1 sector
```

---

## 13. Checklist for Completion

For each subsystem, verify:

- [ ] Struct defined with `#[derive(Debug, Clone, Serialize, Deserialize)]`
- [ ] Collection function handles all error paths with `Option` or graceful fallback
- [ ] Unit tests with hardcoded sample data
- [ ] CLI subcommand or flag added (if applicable)
- [ ] `app.rs` orchestration function (`run_*`)
- [ ] `ui.rs` render function
- [ ] Doctor check added for capability detection
- [ ] Integrated into `SessionRecord` or `HealthReport`
- [ ] JSON/CSV/HTML export updated
- [ ] Safety checks enforced for destructive operations
- [ ] Caveats included for inferred/estimated data
- [ ] `lib.rs` module declaration added (for new files)
- [ ] Cargo.toml updated (if new dependencies)
