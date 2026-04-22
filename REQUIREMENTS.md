# emmc-lab: Open Requirements

> Status snapshot — April 2026. Based on codebase audit at v0.2.8.
> Completed items are listed at the bottom for reference.

---

## What Is Already Shipped

| Feature | Module | Status |
|---------|--------|--------|
| Workload engine (io_uring + pread/pwrite) | engine.rs | ✅ |
| FIEMAP / FIBMAP file extent mapping | filemap.rs | ✅ |
| LBA tracing via blktrace / blkparse | lba_trace.rs | ✅ |
| Health compare vs failure-analysis reports | health_compare.rs | ✅ |
| EXT_CSD basic health fields | health.rs | ✅ |
| CID register parsing | health.rs | ✅ |
| Vendor health data (Samsung/SanDisk/Micron/Hynix) | health.rs | ✅ |
| Read-disturb model (partial) | health.rs | ✅ |
| /proc/diskstats raw parser | system.rs | ✅ |
| CLI subcommands: filemap, lba-trace, health-compare | cli.rs | ✅ |
| JSON / CSV / HTML session export | report.rs | ✅ |
| Interactive terminal UI | ui.rs | ✅ |
| Header tail-preserving truncation fix | ui.rs | ✅ |

---

## Open Requirements

### REQ-1 — Block-Device Geometry

**What**: A `BlockGeometry` struct and `collect_geometry(device)` function that reads all
block-layer geometry attributes from sysfs in one pass.

**New file**: `src/geometry.rs` + declare in `lib.rs`

**Fields to collect** (all from `/sys/class/block/{name}/`):

| sysfs path | Field | Notes |
|------------|-------|-------|
| `queue/logical_block_size` | `logical_block_size: u64` | Already in DeviceInfo — unify |
| `queue/physical_block_size` | `physical_block_size: u64` | Not yet collected |
| `queue/minimum_io_size` | `minimum_io_size: u64` | |
| `queue/optimal_io_size` | `optimal_io_size: u64` | 0 means "not specified" |
| `queue/discard_granularity` | `discard_granularity: u64` | 0 = no discard |
| `queue/discard_max_bytes` | `discard_max_bytes: u64` | |
| `queue/write_cache` | `write_cache: Option<String>` | "write back" / "write through" |
| `queue/scheduler` | `scheduler: Option<String>` | Extract bracketed active value |
| `alignment_offset` | `alignment_offset: u64` | |
| `start` | `partition_start_lba: Option<u64>` | Partitions only |
| `ro` | `read_only: bool` | |
| `removable` | `removable: bool` | Already in DeviceInfo |
| `size` | `size_sectors: u64` | |

**Notes for the implementer**:
- Queue attributes live on the parent device for partitions (e.g., `mmcblk0p1` → read from `mmcblk0/queue/`).
  Detect this: if `/sys/class/block/{name}/queue/` is empty/missing, strip trailing digits to find parent.
- Parse `write_cache` and `scheduler` strings by extracting the bracketed token if present,
  otherwise use the full trimmed value.
- Every field must use `Option<T>` or a 0-default — never panic on missing sysfs entries.

**Integration**:
- Add `pub geometry: Option<BlockGeometry>` to `DeviceInfo`
- Call from `list_devices()` in `system.rs`
- Add `render_geometry()` in `ui.rs`
- New CLI subcommand: `emmc-lab geometry --device /dev/mmcblk0`
- Add to doctor checks: verify `physical_block_size` is readable
- Warn in engine if profile `block_size_bytes` isn't a multiple of `physical_block_size`
- Warn if addressing range isn't aligned to `optimal_io_size` (when non-zero)

---

### REQ-2 — TRIM / Discard Support

**What**: Detection, probing, and optional controlled discard of test regions.

**New file**: `src/discard.rs` + declare in `lib.rs`

**Struct**:
```
DiscardCapability {
    device: PathBuf,
    discard_supported: bool,        // discard_granularity > 0
    discard_granularity: u64,
    discard_max_bytes: u64,
    blkdiscard_available: bool,     // command_exists("blkdiscard")
    fstrim_available: bool,         // command_exists("fstrim")
    probe_result: Option<DiscardProbeResult>,
}

DiscardProbeResult {
    probe_offset: u64,
    probe_length: u64,
    success: bool,
    duration_us: u64,
    error: Option<String>,
}
```

**BLKDISCARD ioctl**:
```
const BLKDISCARD: libc::c_ulong = 0x1277;   // _IO(0x12, 119)
let range: [u64; 2] = [offset, length];
libc::ioctl(fd, BLKDISCARD, &range)
```

**Safety — ALL must pass before issuing BLKDISCARD**:
1. Device must not be mounted (check `/proc/self/mountinfo`)
2. Device must not be the root/boot device
3. Caller must pass `--i-understand-this-will-discard-data` flag
4. `offset` and `length` must be aligned to `discard_granularity`
5. `length` must not exceed `discard_max_bytes`

**FITRIM detection** (filesystem-level, mounted path):
- Open the mountpoint directory fd and check if `FITRIM` ioctl is viable
- Detection only — do NOT issue FITRIM without explicit user opt-in

**CLI**:
```
emmc-lab discard --device /dev/mmcblk0
    → prints DiscardCapability (detection only, safe, no root needed)

emmc-lab discard --device /dev/mmcblk0 --probe
    → safe probe on one discard_granularity-sized region (root, unmounted, no destruction flag needed for single-probe)

emmc-lab discard --device /dev/mmcblk0 --test-range 0-67108864 --i-understand-this-will-discard-data
    → controlled discard latency test across a byte range (destructive, requires confirmation)
```

**Integration**:
- Include `DiscardCapability` in `HealthReport` and `SessionRecord`
- Doctor command: add discard capability check
- Geometry display: show discard fields prominently (granularity, max bytes)
- LBA trace already captures `IoDirection::Discard` — cross-reference in reports

---

### REQ-3 — Full EXT_CSD Decoding

**What**: Parse all operationally relevant fields from the 512-byte EXT_CSD register,
not just the current ~15. The raw bytes are already available in `EmmcHealthSnapshot.ext_csd_raw`.

**Extend**: `health.rs` — add `ExtCsdDecoded` struct and `decode_ext_csd(raw: &[u8; 512])` function.

**Fields to add** (byte offsets per JEDEC JESD84-B51):

| Byte | Field | Description |
|------|-------|-------------|
| 226 | `boot_size_mult` | Boot partition size = 128 KB × value |
| 168 | `rpmb_size_mult` | RPMB size = 128 KB × value |
| 179 | `partition_config` | Boot partition enable + access bits |
| 177 | `boot_bus_conditions` | Boot bus width and mode |
| 162 | `rst_n_function` | Hardware reset function |
| 160 | `partitioning_support` | Partitioning feature support flags |
| 163 | `bkops_en` | Background operations enable |
| 164 | `bkops_start` | Trigger manual BKOPS |
| 175 | `erase_group_def` | Use high-capacity erase group sizes |
| 221 | `hc_wp_grp_size` | Write-protect group size |
| 222 | `rel_wr_sec_c` | Reliable write sector count |
| 167 | `wr_rel_set` | Write reliability setting |
| 166 | `wr_rel_param` | Write reliability parameter |
| 185 | `hs_timing` | High-speed timing mode |
| 183 | `bus_width` | Bus width register |
| 34 | `power_off_notification` | Power-off notification enable |
| 247 | `power_off_long_time` | Power-off long time (ms) |
| 308 | `cmdq_support` | Command queue support |
| 307 | `cmdq_depth` | Command queue depth |
| 16 | `secure_removal_type` | Secure removal type |
| 492 | `ffu_features` | Field firmware update features |

**Derived display values** to expose in UI:

```
boot_partition_1_size_kb = boot_size_mult * 128
boot_partition_2_size_kb = boot_size_mult * 128   (symmetric)
rpmb_size_kb             = rpmb_size_mult * 128
boot_partition_enabled   = (partition_config >> 3) & 0x7 != 0
access_partition         = partition_config & 0x7
```

**Health bucket decoders** (these already partially exist — standardise them):

```
decode_life_time_estimate(u8) -> &'static str
    0x00 → "Not defined"
    0x01 → "0%–10% device life used"
    0x02 → "10%–20% device life used"
    ...
    0x0A → "90%–100% device life used"
    0x0B → "Exceeded estimated maximum life"

decode_pre_eol(u8) -> &'static str
    0x00 → "Not defined"
    0x01 → "Normal"
    0x02 → "Warning — 80% of reserved blocks consumed"
    0x03 → "Urgent — 90% of reserved blocks consumed"
```

**Integration**: Update `render_health_report()` in `ui.rs` to show decoded values for all new fields,
grouped under clear section headings: Boot & RPMB, Partitioning, Cache & BKOPS, Reliability, Power, Command Queue.

---

### REQ-4 — CSD Register Parsing

**What**: Parse the CSD register into a typed struct. The raw value is available at
`/sys/class/block/mmcblk{N}/device/csd`.

**New struct** in `health.rs`:
```
CsdInfo {
    csd_structure: u8,      // CSD version (3 for eMMC ≥4.1)
    spec_vers: u8,
    taac: u8,               // Data read access time
    nsac: u8,               // Access time in CLK cycles
    tran_speed: u8,         // Max bus clock frequency code
    ccc: u16,               // Card command classes bitmask
    read_bl_len: u8,
    c_size: u32,            // Device size field
    erase_grp_size: u8,
    erase_grp_mult: u8,
    wp_grp_size: u8,
    r2w_factor: u8,
    write_bl_len: u8,
    perm_write_protect: bool,
    tmp_write_protect: bool,
    raw_hex: String,
}
```

Read from sysfs: `/sys/class/block/{device_name}/device/csd`
The sysfs value is a hex string (e.g., `"d0270032..."`). Parse into bytes then decode bitfields
per JEDEC JESD84-B51 section 8.3.

**Integration**: Add `csd: Option<CsdInfo>` to `EmmcHealthSnapshot`.

---

### REQ-5 — Richer Block-Level Stats

**What**: Derive throughput, queue depth, await latencies, and merge ratios from
`/proc/diskstats` via delta computation between two snapshots.

**New structs** in `diagnostics.rs`:

```
BlockStats {
    device: String,
    timestamp: DateTime<Utc>,
    // Raw counters — all 17 diskstats fields where available:
    reads_completed: u64,
    reads_merged: u64,
    sectors_read: u64,
    read_time_ms: u64,
    writes_completed: u64,
    writes_merged: u64,
    sectors_written: u64,
    write_time_ms: u64,
    ios_in_progress: u64,
    io_time_ms: u64,
    weighted_io_time_ms: u64,
    discards_completed: Option<u64>,   // kernel 4.18+
    discards_merged: Option<u64>,
    sectors_discarded: Option<u64>,
    discard_time_ms: Option<u64>,
    flush_requests: Option<u64>,       // kernel 5.5+
    flush_time_ms: Option<u64>,
}

BlockStatsDelta {
    device: String,
    interval_ms: u64,
    // Rates
    read_iops: f64,
    write_iops: f64,
    discard_iops: Option<f64>,
    read_throughput_bytes_per_sec: f64,
    write_throughput_bytes_per_sec: f64,
    // Merge ratios (0.0–1.0; higher = more merging)
    read_merge_ratio: f64,
    write_merge_ratio: f64,
    // Latency estimates
    avg_read_await_ms: f64,
    avg_write_await_ms: f64,
    avg_discard_await_ms: Option<f64>,
    avg_flush_await_ms: Option<f64>,
    // Queue / utilisation
    avg_queue_depth: f64,           // weighted_io_time_ms / interval_ms
    utilization_percent: f64,       // io_time_ms / interval_ms × 100, capped at 100
    raw: BlockStats,
}
```

**Key formulas**:
```
read_iops                    = Δreads_completed   / (interval_ms / 1000)
read_throughput              = Δsectors_read × sector_size / (interval_ms / 1000)
read_merge_ratio             = Δreads_merged / (Δreads_merged + Δreads_completed)  [guard ÷0]
avg_read_await_ms            = Δread_time_ms / Δreads_completed                    [guard ÷0]
avg_queue_depth              = Δweighted_io_time_ms / interval_ms
utilization_percent          = (Δio_time_ms / interval_ms) × 100
```

**New function**:
```rust
pub fn read_block_stats(device: &str) -> Result<BlockStats>
pub fn compute_block_stats_delta(prev: &BlockStats, curr: &BlockStats, interval_ms: u64) -> BlockStatsDelta
```

Integrate into `LiveSamplerState::tick()` — take a snapshot each tick, compute delta,
append to `DiagnosticReport.block_stats_timeline: Vec<BlockStatsDelta>`.

**No new CLI subcommand needed** — enrich `diag monitor` and `diag sample` output automatically.

---

### REQ-6 — Trace Event Analytics

**What**: Aggregate blktrace events into summary statistics and an LBA heatmap.
Currently `LbaTraceReport` stores raw events but has no aggregation.

**New structs** in `lba_trace.rs`:

```
LbaHeatmapBucket {
    start_lba: u64,
    end_lba: u64,
    read_count: u64,
    write_count: u64,
    discard_count: u64,
}

KernelIoBucket {
    source: String,             // "jbd2", "kworker", "mmcqd", "unknown-kernel"
    read_events: u64,
    write_events: u64,
    sectors_read: u64,
    sectors_written: u64,
}

TraceEventSummary {
    total_events: u64,
    read_events: u64,
    write_events: u64,
    discard_events: u64,
    flush_events: u64,
    peak_iops_read: f64,
    peak_iops_write: f64,
    attributed_events: u64,
    unattributed_events: u64,
    kernel_buckets: Vec<KernelIoBucket>,
    lba_heatmap: Vec<LbaHeatmapBucket>,   // divide device into N=32 equal buckets
}
```

**Kernel I/O detection**: blktrace events from PIDs matching `kworker`, `jbd2`, `mmcqd`,
`ksoftirqd`, `kswapd`, or PID 0 should be tagged as kernel I/O. Report these in `kernel_buckets`
separately — they are not attributable to userspace processes.

**Heatmap**: Divide the device logical range into 32 equal buckets. For each trace event,
find the bucket index (`lba × 32 / total_sectors`) and increment the appropriate counter.

**Integration**: Add `summary: Option<TraceEventSummary>` to `LbaTraceReport`.
Compute it at the end of `capture_lba_trace()`. Display in the LBA trace terminal output.

---

### REQ-7 — Endurance Model Struct

**What**: Replace the current informal `ReadDisturbModel` in `health.rs` with a fully typed
`EnduranceModel` that takes both geometry and host-visible I/O counters as inputs.

```
EnduranceModel {
    // Inputs
    device_capacity_bytes: u64,
    erase_block_size_bytes: u64,
    page_size_bytes: u64,
    pages_per_block: u64,
    total_sectors_written: u64,     // from diskstats or session total
    total_sectors_read: u64,

    // Derived
    total_erase_blocks: u64,
    estimated_pe_cycles: f64,           // total_written / capacity (host-visible minimum)
    reads_per_block_estimate: f64,      // total_read / total_erase_blocks
    read_disturb_ratio: f64,            // reads_per_block / threshold
    read_disturb_risk: ReadDisturbRisk, // Low / Moderate / Elevated / High / Unknown

    // Caveats — ALWAYS populated, ALWAYS shown in output
    caveats: Vec<String>,
}
```

**Mandatory caveat strings** (must always appear in every output path — terminal, JSON, HTML):
- `"All figures are host-visible logical I/O counts, not physical NAND operations."`
- `"FTL wear-leveling, garbage collection, and write amplification are opaque to the host."`
- `"Actual NAND wear may be 2–10× higher than host-visible writes suggest."`
- `"Read-disturb thresholds vary by NAND technology (SLC/MLC/TLC/QLC) and are not readable from the device."`

---

### REQ-8 — eBPF / bpftrace Integration

**What**: Use bpftrace as a higher-fidelity alternative to procfs sampling for the
`diag trace` command, with graceful fallback.

**Detection** (add to `system.rs`):
```
TraceCapabilities {
    bpftrace_available: bool,     // command_exists("bpftrace")
    blktrace_available: bool,     // command_exists("blktrace") && command_exists("blkparse")
    debugfs_mounted: bool,        // /sys/kernel/debug exists
    tracefs_available: bool,      // /sys/kernel/tracing or /sys/kernel/debug/tracing
    is_root: bool,
}
```

**bpftrace script** (if bpftrace ≥ 0.11 is available):
```
tracepoint:block:block_rq_complete {
    printf("%llu %d %s %llu %u\n",
           nsecs, pid, comm, args->sector, args->nr_sector);
}
```
Run via `bpftrace -e '...' -c "sleep {duration}"`, parse stdout into `TraceEvent` structs.

**Fallback chain**: `bpftrace → blktrace/blkparse → procfs sampler`
Try the best available, fall back transparently. Always report which backend was used.

**Note**: Only implement read-only tracepoints. Never attach probes that could modify
kernel state or issue I/O.

---

## Cross-Cutting Rules (for all of the above)

**Error handling**: Every sysfs/procfs read must use `Option<T>` or return gracefully.
Pattern: `fs::read_to_string(path).ok()?.trim().parse().ok()`

**Serialisation**: Every new struct must derive `#[derive(Debug, Clone, Serialize, Deserialize)]`.

**Graceful degradation**: Every feature must have three operating modes:
1. Full — all prerequisites met
2. Partial — some data available
3. Unavailable — feature clearly marked as unavailable with a reason string

**Honesty contract** — never claim in any output:
- Physical NAND page/block targeting
- Exact NAND wear per cell or block
- Exact post-FTL data placement
- Exact write amplification (estimates only, with caveats)

**Safety**: No destructive ioctl (`BLKDISCARD`, `BLKZEROOUT`, MMC write commands) without:
- Device unmounted
- Not a root device
- Explicit confirmation flag

---

## Recommended Implementation Order

```
Phase 1 — Foundation
  REQ-1  Block-device geometry   ← everything else uses BlockGeometry
  REQ-5  Richer block stats      ← needed for endurance model

Phase 2 — Core telemetry
  REQ-3  Full EXT_CSD decoding   ← straightforward byte-offset work
  REQ-4  CSD register parsing    ← sysfs hex string decode

Phase 3 — Analysis
  REQ-6  Trace event analytics   ← build on existing LbaTraceReport
  REQ-7  Endurance model struct  ← pulls from geometry + block stats

Phase 4 — Advanced
  REQ-2  TRIM / discard support  ← needs geometry for alignment checks
  REQ-8  eBPF integration        ← optional, most complex
```
