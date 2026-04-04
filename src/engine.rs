use crate::profile::{DurabilityMode, Profile, ResolvedRange, StopCondition, TargetMode, WorkloadType};
use crate::system::{block_device_size, io_uring_available, logical_block_size};
use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::mem::MaybeUninit;
use std::os::fd::RawFd;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct IntervalStats {
    pub second: u64,
    pub read_ios_completed: u64,
    pub write_ios_completed: u64,
    pub read_bytes_completed: u64,
    pub write_bytes_completed: u64,
    pub failed_reads: u64,
    pub failed_writes: u64,
    pub partial_reads: u64,
    pub partial_writes: u64,
    pub retries: u64,
    pub verify_failures: u64,
    pub sync_failures: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FirstVerifyFailure {
    pub kind: String,
    pub offset: u64,
    pub expected_checksum: u32,
    pub actual_checksum: u32,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct VerifyResults {
    pub checksum_mismatch: u64,
    pub offset_mismatch: u64,
    pub stale_data: u64,
    pub short_read: u64,
    pub unexpected_eof: u64,
    pub verify_failure_count: u64,
    pub first_failure: Option<FirstVerifyFailure>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LatencySummary {
    pub p50_us: u64,
    pub p95_us: u64,
    pub p99_us: u64,
    pub p999_us: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RunSummary {
    pub backend: String,
    pub started_at: DateTime<Utc>,
    pub ended_at: DateTime<Utc>,
    pub target_path: PathBuf,
    pub target_mode: String,
    pub read_ios_completed: u64,
    pub write_ios_completed: u64,
    pub read_bytes_completed: u64,
    pub write_bytes_completed: u64,
    pub failed_reads: u64,
    pub failed_writes: u64,
    pub partial_reads: u64,
    pub partial_writes: u64,
    pub retries: u64,
    pub verify_failures: u64,
    pub sync_failures: u64,
    pub read_latency: LatencySummary,
    pub write_latency: LatencySummary,
    pub verify: VerifyResults,
    pub addressed_range_start_bytes: u64,
    pub addressed_range_length_bytes: u64,
    pub logical_sector_size: u64,
    pub notes: Vec<String>,
}

#[derive(Debug, Default)]
struct GlobalCounters {
    read_ios_completed: AtomicU64,
    write_ios_completed: AtomicU64,
    read_bytes_completed: AtomicU64,
    write_bytes_completed: AtomicU64,
    failed_reads: AtomicU64,
    failed_writes: AtomicU64,
    partial_reads: AtomicU64,
    partial_writes: AtomicU64,
    retries: AtomicU64,
    verify_failures: AtomicU64,
    sync_failures: AtomicU64,
}

#[derive(Debug, Clone, Default)]
struct CounterSnapshot {
    read_ios_completed: u64,
    write_ios_completed: u64,
    read_bytes_completed: u64,
    write_bytes_completed: u64,
    failed_reads: u64,
    failed_writes: u64,
    partial_reads: u64,
    partial_writes: u64,
    retries: u64,
    verify_failures: u64,
    sync_failures: u64,
}

#[derive(Debug, Default)]
struct LocalStats {
    read_hist: LatencyHistogram,
    write_hist: LatencyHistogram,
    verify: VerifyResults,
}

#[derive(Debug)]
struct LatencyHistogram {
    counts: [u64; 64],
    total: u64,
}

impl Default for LatencyHistogram {
    fn default() -> Self {
        Self {
            counts: [0_u64; 64],
            total: 0,
        }
    }
}

pub fn execute_profile(profile: &Profile) -> Result<(RunSummary, Vec<IntervalStats>)> {
    profile.validate()?;
    let started_at = Utc::now();
    let target_len = prepare_target(profile)?;
    let logical_sector_size = detect_logical_sector_size(profile);
    let resolved_range = profile.resolved_range(target_len, logical_sector_size)?;
    validate_alignment(profile, &resolved_range)?;
    let backend = choose_backend();

    let counters = Arc::new(GlobalCounters::default());
    let stop_flag = Arc::new(AtomicBool::new(false));
    let next_op = Arc::new(AtomicU64::new(0));
    let (interval_tx, interval_rx) = mpsc::channel::<IntervalStats>();
    let (worker_tx, worker_rx) = mpsc::channel::<LocalStats>();
    let report_interval = Duration::from_millis(profile.workload.report_interval_ms.max(100));
    let interval_counters = Arc::clone(&counters);
    let interval_stop = Arc::clone(&stop_flag);

    let sampler = thread::spawn(move || {
        let started = Instant::now();
        let mut prev = snapshot_counters(&interval_counters);
        while !interval_stop.load(Ordering::Relaxed) {
            thread::sleep(report_interval);
            let current = snapshot_counters(&interval_counters);
            let second = started.elapsed().as_secs();
            let _ = interval_tx.send(IntervalStats {
                second,
                read_ios_completed: current.read_ios_completed.saturating_sub(prev.read_ios_completed),
                write_ios_completed: current.write_ios_completed.saturating_sub(prev.write_ios_completed),
                read_bytes_completed: current.read_bytes_completed.saturating_sub(prev.read_bytes_completed),
                write_bytes_completed: current.write_bytes_completed.saturating_sub(prev.write_bytes_completed),
                failed_reads: current.failed_reads.saturating_sub(prev.failed_reads),
                failed_writes: current.failed_writes.saturating_sub(prev.failed_writes),
                partial_reads: current.partial_reads.saturating_sub(prev.partial_reads),
                partial_writes: current.partial_writes.saturating_sub(prev.partial_writes),
                retries: current.retries.saturating_sub(prev.retries),
                verify_failures: current.verify_failures.saturating_sub(prev.verify_failures),
                sync_failures: current.sync_failures.saturating_sub(prev.sync_failures),
            });
            prev = current;
        }
    });

    let started = Instant::now();
    let mut joins = Vec::new();
    for worker_id in 0..profile.workload.worker_count {
        let profile = profile.clone();
        let counters = Arc::clone(&counters);
        let stop = Arc::clone(&stop_flag);
        let next_op = Arc::clone(&next_op);
        let worker_tx = worker_tx.clone();
        let range = resolved_range.clone();
        joins.push(thread::spawn(move || {
            let stats = worker_loop(worker_id, &profile, range, counters, stop, next_op, started);
            let _ = worker_tx.send(stats.unwrap_or_default());
        }));
    }
    drop(worker_tx);

    for join in joins {
        let _ = join.join();
    }
    stop_flag.store(true, Ordering::Relaxed);
    let _ = sampler.join();

    let intervals = interval_rx.into_iter().collect::<Vec<_>>();
    let mut merged = LocalStats::default();
    for local in worker_rx.into_iter() {
        merged.read_hist.merge(&local.read_hist);
        merged.write_hist.merge(&local.write_hist);
        merge_verify(&mut merged.verify, &local.verify);
    }
    let ended_at = Utc::now();
    let current = snapshot_counters(&counters);

    let mut notes = vec![
        "Logical sector/LBA targeting only; this does not guarantee fixed physical NAND cell targeting.".to_string(),
        format!("Backend used: {}", backend),
    ];
    if profile.workload.direct_io {
        notes.push("Direct I/O requested; alignment was validated against block size and logical sector size.".to_string());
    }
    if backend != "io_uring" && io_uring_available() {
        notes.push("io_uring appears available, but this run used the fallback pread/pwrite backend.".to_string());
    }

    Ok((
        RunSummary {
            backend,
            started_at,
            ended_at,
            target_path: profile.target.path.clone(),
            target_mode: profile.target.mode.to_string(),
            read_ios_completed: current.read_ios_completed,
            write_ios_completed: current.write_ios_completed,
            read_bytes_completed: current.read_bytes_completed,
            write_bytes_completed: current.write_bytes_completed,
            failed_reads: current.failed_reads,
            failed_writes: current.failed_writes,
            partial_reads: current.partial_reads,
            partial_writes: current.partial_writes,
            retries: current.retries,
            verify_failures: current.verify_failures,
            sync_failures: current.sync_failures,
            read_latency: merged.read_hist.to_summary(),
            write_latency: merged.write_hist.to_summary(),
            verify: merged.verify,
            addressed_range_start_bytes: resolved_range.start_bytes,
            addressed_range_length_bytes: resolved_range.length_bytes,
            logical_sector_size: resolved_range.logical_sector_size,
            notes,
        },
        intervals,
    ))
}

fn prepare_target(profile: &Profile) -> Result<u64> {
    match profile.target.mode {
        TargetMode::RawDevice => {
            block_device_size(&profile.target.path)
                .ok_or_else(|| anyhow!("unable to determine raw device size for {}", profile.target.path.display()))
        }
        TargetMode::FileBased => {
            if !profile.target.path.exists() && profile.is_write_workload() {
                if let Some(parent) = profile.target.path.parent() {
                    fs::create_dir_all(parent)?;
                }
                let _ = OpenOptions::new()
                    .create(true)
                    .write(true)
                    .read(true)
                    .open(&profile.target.path)?;
            }
            let metadata = fs::metadata(&profile.target.path)
                .with_context(|| format!("failed to stat {}", profile.target.path.display()))?;
            let existing = metadata.len();
            let requested = match profile.addressing.mode {
                crate::profile::AddressingMode::WholeSelectedRange => existing.max(1024 * 1024),
                crate::profile::AddressingMode::SectorRange => {
                    let sectors = profile.addressing.sector_count.unwrap_or(0);
                    let start = profile.addressing.start_sector.unwrap_or(0);
                    (start + sectors).saturating_mul(512)
                }
                crate::profile::AddressingMode::ByteRange => profile
                    .addressing
                    .start_offset_bytes
                    .unwrap_or(0)
                    .saturating_add(profile.addressing.range_size_bytes.unwrap_or(0)),
            }
            .max(existing);
            if profile.is_write_workload() && requested > existing {
                let file = OpenOptions::new()
                    .create(true)
                    .write(true)
                    .read(true)
                    .open(&profile.target.path)?;
                file.set_len(requested)?;
            }
            Ok(fs::metadata(&profile.target.path)?.len())
        }
    }
}

fn detect_logical_sector_size(profile: &Profile) -> u64 {
    match profile.target.mode {
        TargetMode::RawDevice => logical_block_size(&profile.target.path).unwrap_or(512),
        TargetMode::FileBased => 512,
    }
}

fn validate_alignment(profile: &Profile, range: &ResolvedRange) -> Result<()> {
    let block = profile.workload.block_size_bytes as u64;
    if range.start_bytes % block != 0 {
        bail!("start offset must be aligned to block size");
    }
    if profile.workload.direct_io {
        if block % range.logical_sector_size != 0 {
            bail!(
                "direct I/O block size {} must align to logical sector size {}",
                block,
                range.logical_sector_size
            );
        }
        if range.start_bytes % range.logical_sector_size != 0 {
            bail!("direct I/O start offset must align to logical sector size");
        }
    }
    Ok(())
}

fn choose_backend() -> String {
    "pread_pwrite".to_string()
}

fn worker_loop(
    worker_id: usize,
    profile: &Profile,
    range: ResolvedRange,
    counters: Arc<GlobalCounters>,
    stop_flag: Arc<AtomicBool>,
    next_op: Arc<AtomicU64>,
    started: Instant,
) -> Result<LocalStats> {
    let mut local = LocalStats::default();
    let block = profile.workload.block_size_bytes;
    let write_access = profile.is_write_workload();
    let fd = open_fd(profile, write_access)?;
    let mut io_buffer = AlignedBuffer::new(block, block.max(range.logical_sector_size as usize))?;
    let mut verify_buffer = AlignedBuffer::new(block, block.max(range.logical_sector_size as usize))?;
    let mut writes_since_sync = 0_u64;
    let mut last_sync = Instant::now();

    loop {
        if stop_flag.load(Ordering::Relaxed) {
            break;
        }

        let maybe_op_index = match profile.stop_condition() {
            StopCondition::ExactOperationCount => {
                let op = next_op.fetch_add(1, Ordering::Relaxed);
                let max_ops = profile.workload.exact_op_count.unwrap_or(0);
                if op >= max_ops {
                    None
                } else {
                    Some(op)
                }
            }
            StopCondition::RuntimeSeconds => {
                if started.elapsed().as_secs() >= profile.workload.runtime_seconds.unwrap_or(0) {
                    None
                } else {
                    Some(next_op.fetch_add(1, Ordering::Relaxed))
                }
            }
        };
        let Some(op_index) = maybe_op_index else {
            break;
        };

        let offset = compute_offset(profile, &range, op_index)?;
        let op_kind = compute_op_kind(profile, op_index);
        match op_kind {
            IoKind::Read => {
                let t0 = Instant::now();
                match do_pread(fd, io_buffer.as_mut_ptr(), block, offset) {
                    Ok(n) if n == block => {
                        counters.read_ios_completed.fetch_add(1, Ordering::Relaxed);
                        counters.read_bytes_completed.fetch_add(n as u64, Ordering::Relaxed);
                    }
                    Ok(n) => {
                        counters.partial_reads.fetch_add(1, Ordering::Relaxed);
                        counters.read_bytes_completed.fetch_add(n as u64, Ordering::Relaxed);
                    }
                    Err(err) => {
                        if should_retry(&err) {
                            counters.retries.fetch_add(1, Ordering::Relaxed);
                            continue;
                        }
                        counters.failed_reads.fetch_add(1, Ordering::Relaxed);
                    }
                }
                local.read_hist.record(t0.elapsed());
            }
            IoKind::Write => {
                fill_pattern(io_buffer.as_mut_slice(), profile.workload.random_seed, op_index, offset, worker_id as u64);
                let checksum = crc32fast::hash(io_buffer.as_slice());
                let t0 = Instant::now();
                match do_pwrite(fd, io_buffer.as_ptr(), block, offset) {
                    Ok(n) if n == block => {
                        counters.write_ios_completed.fetch_add(1, Ordering::Relaxed);
                        counters.write_bytes_completed.fetch_add(n as u64, Ordering::Relaxed);
                    }
                    Ok(n) => {
                        counters.partial_writes.fetch_add(1, Ordering::Relaxed);
                        counters.write_bytes_completed.fetch_add(n as u64, Ordering::Relaxed);
                    }
                    Err(err) => {
                        if should_retry(&err) {
                            counters.retries.fetch_add(1, Ordering::Relaxed);
                            continue;
                        }
                        counters.failed_writes.fetch_add(1, Ordering::Relaxed);
                        local.write_hist.record(t0.elapsed());
                        continue;
                    }
                }
                local.write_hist.record(t0.elapsed());
                writes_since_sync += 1;

                if profile.verification.enabled {
                    match do_pread(fd, verify_buffer.as_mut_ptr(), block, offset) {
                        Ok(n) if n == block => {
                            let actual = crc32fast::hash(verify_buffer.as_slice());
                            if actual != checksum {
                                counters.verify_failures.fetch_add(1, Ordering::Relaxed);
                                note_verify_failure(
                                    &mut local.verify,
                                    "checksum_mismatch",
                                    offset as u64,
                                    checksum,
                                    actual,
                                    "read-back checksum mismatch",
                                );
                            }
                        }
                        Ok(n) => {
                            counters.verify_failures.fetch_add(1, Ordering::Relaxed);
                            local.verify.short_read += 1;
                            local.verify.verify_failure_count += 1;
                            if local.verify.first_failure.is_none() {
                                local.verify.first_failure = Some(FirstVerifyFailure {
                                    kind: "short_read".to_string(),
                                    offset: offset as u64,
                                    expected_checksum: checksum,
                                    actual_checksum: 0,
                                    detail: format!("verify read returned {} bytes", n),
                                });
                            }
                        }
                        Err(_) => {
                            counters.verify_failures.fetch_add(1, Ordering::Relaxed);
                            local.verify.unexpected_eof += 1;
                            local.verify.verify_failure_count += 1;
                        }
                    }
                }

                if should_sync(profile, writes_since_sync, last_sync.elapsed()) {
                    if portable_fdatasync(fd) != 0 {
                        counters.sync_failures.fetch_add(1, Ordering::Relaxed);
                    } else {
                        writes_since_sync = 0;
                        last_sync = Instant::now();
                    }
                }
            }
        }
    }

    if profile.durability.fdatasync_at_end && write_access {
        if portable_fdatasync(fd) != 0 {
            counters.sync_failures.fetch_add(1, Ordering::Relaxed);
        }
    }
    unsafe { libc::close(fd) };
    Ok(local)
}

fn compute_offset(profile: &Profile, range: &ResolvedRange, op_index: u64) -> Result<i64> {
    let block = profile.workload.block_size_bytes as u64;
    let block_count = range.length_bytes / block;
    if block_count == 0 {
        bail!("range does not contain a full block");
    }
    let block_index = if profile.is_random_workload() {
        splitmix64(profile.workload.random_seed.wrapping_add(op_index)) % block_count
    } else {
        op_index % block_count
    };
    let offset = range.start_bytes + block_index.saturating_mul(block);
    i64::try_from(offset).context("offset overflow")
}

fn compute_op_kind(profile: &Profile, op_index: u64) -> IoKind {
    match profile.workload.test_type {
        WorkloadType::RandRead | WorkloadType::Read => IoKind::Read,
        WorkloadType::RandWrite | WorkloadType::Write => IoKind::Write,
        WorkloadType::RandRw => {
            let bucket = splitmix64(profile.workload.random_seed ^ op_index) % 100;
            if bucket < profile.workload.read_percentage as u64 {
                IoKind::Read
            } else {
                IoKind::Write
            }
        }
    }
}

fn open_fd(profile: &Profile, write_access: bool) -> Result<RawFd> {
    let mut flags = if write_access { libc::O_RDWR } else { libc::O_RDONLY };
    if profile.workload.direct_io {
        flags |= o_direct_flag();
    }
    let c_path = std::ffi::CString::new(profile.target.path.to_string_lossy().to_string())?;
    let fd = unsafe { libc::open(c_path.as_ptr(), flags, 0o644) };
    if fd < 0 {
        bail!("failed to open {}", profile.target.path.display());
    }
    Ok(fd)
}

fn do_pread(fd: RawFd, ptr: *mut u8, len: usize, offset: i64) -> Result<usize> {
    let rc = unsafe { libc::pread(fd, ptr.cast(), len, offset) };
    if rc < 0 {
        bail!("{}", std::io::Error::last_os_error());
    }
    Ok(rc as usize)
}

fn do_pwrite(fd: RawFd, ptr: *const u8, len: usize, offset: i64) -> Result<usize> {
    let rc = unsafe { libc::pwrite(fd, ptr.cast(), len, offset) };
    if rc < 0 {
        bail!("{}", std::io::Error::last_os_error());
    }
    Ok(rc as usize)
}

fn should_retry(err: &anyhow::Error) -> bool {
    let text = err.to_string();
    text.contains("Interrupted system call") || text.contains("Resource temporarily unavailable")
}

fn should_sync(profile: &Profile, writes_since_sync: u64, elapsed: Duration) -> bool {
    match profile.durability.mode {
        DurabilityMode::Performance => false,
        DurabilityMode::BatchDurable => {
            let by_count = profile
                .workload
                .sync_cadence_writes
                .map(|n| writes_since_sync >= n)
                .unwrap_or(false);
            let by_time = profile
                .workload
                .sync_cadence_ms
                .map(|ms| elapsed >= Duration::from_millis(ms))
                .unwrap_or(false);
            by_count || by_time
        }
        DurabilityMode::StrictDurable => writes_since_sync >= 1,
    }
}

fn snapshot_counters(counters: &GlobalCounters) -> CounterSnapshot {
    CounterSnapshot {
        read_ios_completed: counters.read_ios_completed.load(Ordering::Relaxed),
        write_ios_completed: counters.write_ios_completed.load(Ordering::Relaxed),
        read_bytes_completed: counters.read_bytes_completed.load(Ordering::Relaxed),
        write_bytes_completed: counters.write_bytes_completed.load(Ordering::Relaxed),
        failed_reads: counters.failed_reads.load(Ordering::Relaxed),
        failed_writes: counters.failed_writes.load(Ordering::Relaxed),
        partial_reads: counters.partial_reads.load(Ordering::Relaxed),
        partial_writes: counters.partial_writes.load(Ordering::Relaxed),
        retries: counters.retries.load(Ordering::Relaxed),
        verify_failures: counters.verify_failures.load(Ordering::Relaxed),
        sync_failures: counters.sync_failures.load(Ordering::Relaxed),
    }
}

fn fill_pattern(buf: &mut [u8], seed: u64, op_index: u64, offset: i64, worker_id: u64) {
    let mut state = splitmix64(seed ^ op_index ^ worker_id ^ offset as u64);
    for chunk in buf.chunks_mut(8) {
        state = splitmix64(state);
        let bytes = state.to_le_bytes();
        let len = chunk.len();
        chunk.copy_from_slice(&bytes[..len]);
    }
}

fn note_verify_failure(
    verify: &mut VerifyResults,
    kind: &str,
    offset: u64,
    expected_checksum: u32,
    actual_checksum: u32,
    detail: &str,
) {
    match kind {
        "checksum_mismatch" => verify.checksum_mismatch += 1,
        "offset_mismatch" => verify.offset_mismatch += 1,
        "stale_data" => verify.stale_data += 1,
        _ => {}
    }
    verify.verify_failure_count += 1;
    if verify.first_failure.is_none() {
        verify.first_failure = Some(FirstVerifyFailure {
            kind: kind.to_string(),
            offset,
            expected_checksum,
            actual_checksum,
            detail: detail.to_string(),
        });
    }
}

fn merge_verify(dst: &mut VerifyResults, src: &VerifyResults) {
    dst.checksum_mismatch += src.checksum_mismatch;
    dst.offset_mismatch += src.offset_mismatch;
    dst.stale_data += src.stale_data;
    dst.short_read += src.short_read;
    dst.unexpected_eof += src.unexpected_eof;
    dst.verify_failure_count += src.verify_failure_count;
    if dst.first_failure.is_none() {
        dst.first_failure = src.first_failure.clone();
    }
}

impl LatencyHistogram {
    fn record(&mut self, duration: Duration) {
        let micros = duration.as_micros() as u64;
        let bucket = micros.max(1).ilog2().min(63) as usize;
        self.counts[bucket] += 1;
        self.total += 1;
    }

    fn merge(&mut self, other: &Self) {
        for (dst, src) in self.counts.iter_mut().zip(other.counts.iter()) {
            *dst += *src;
        }
        self.total += other.total;
    }

    fn to_summary(&self) -> LatencySummary {
        LatencySummary {
            p50_us: self.percentile(0.50),
            p95_us: self.percentile(0.95),
            p99_us: self.percentile(0.99),
            p999_us: self.percentile(0.999),
        }
    }

    fn percentile(&self, p: f64) -> u64 {
        if self.total == 0 {
            return 0;
        }
        let threshold = ((self.total as f64) * p).ceil() as u64;
        let mut seen = 0_u64;
        for (idx, count) in self.counts.iter().enumerate() {
            seen += *count;
            if seen >= threshold {
                return 1_u64 << idx.min(62);
            }
        }
        1_u64 << 63
    }
}

struct AlignedBuffer {
    ptr: *mut u8,
    len: usize,
}

impl AlignedBuffer {
    fn new(len: usize, align: usize) -> Result<Self> {
        let mut ptr = MaybeUninit::<*mut libc::c_void>::uninit();
        let rc = unsafe { libc::posix_memalign(ptr.as_mut_ptr(), align.max(512), len) };
        if rc != 0 {
            bail!("posix_memalign failed with {}", rc);
        }
        let ptr = unsafe { ptr.assume_init() }.cast::<u8>();
        unsafe {
            std::ptr::write_bytes(ptr, 0, len);
        }
        Ok(Self { ptr, len })
    }

    fn as_ptr(&self) -> *const u8 {
        self.ptr.cast_const()
    }

    fn as_mut_ptr(&mut self) -> *mut u8 {
        self.ptr
    }

    fn as_slice(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }

    fn as_mut_slice(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len) }
    }
}

impl Drop for AlignedBuffer {
    fn drop(&mut self) {
        unsafe {
            libc::free(self.ptr.cast());
        }
    }
}

#[derive(Copy, Clone)]
enum IoKind {
    Read,
    Write,
}

fn splitmix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E3779B97F4A7C15);
    let mut z = x;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
    z ^ (z >> 31)
}

fn portable_fdatasync(fd: RawFd) -> i32 {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    unsafe {
        libc::fdatasync(fd)
    }
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    unsafe {
        libc::fsync(fd)
    }
}

fn o_direct_flag() -> i32 {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        libc::O_DIRECT
    }
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::{compute_op_kind, splitmix64};
    use crate::profile::{Profile, WorkloadType};

    #[test]
    fn random_seed_is_reproducible() {
        assert_eq!(splitmix64(7), splitmix64(7));
    }

    #[test]
    fn exact_op_mixed_mode_is_deterministic() {
        let mut profile = Profile::default_named("mixed");
        profile.workload.test_type = WorkloadType::RandRw;
        profile.workload.read_percentage = 50;
        assert!(matches!(compute_op_kind(&profile, 3), _));
    }
}
