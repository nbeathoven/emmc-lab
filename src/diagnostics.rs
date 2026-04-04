use crate::system::{deep_trace_available, diskstats_map, read_link_string, resolve_uid_to_user};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticMode {
    Sample,
    DeepTrace,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProcessSummary {
    pub pid: i32,
    pub ppid: i32,
    pub user: String,
    pub command: String,
    pub exe_path: String,
    pub cwd: String,
    pub logical_read_bytes: u64,
    pub logical_write_bytes: u64,
    pub storage_read_bytes: u64,
    pub storage_write_bytes: u64,
    pub logical_read_bytes_per_sec: f64,
    pub logical_write_bytes_per_sec: f64,
    pub storage_read_bytes_per_sec: f64,
    pub storage_write_bytes_per_sec: f64,
    pub cancelled_write_bytes: u64,
    pub read_syscalls: u64,
    pub write_syscalls: u64,
    pub read_syscalls_per_sec: f64,
    pub write_syscalls_per_sec: f64,
    pub sync_count: u64,
    pub open_file_count: usize,
    pub hottest_file: String,
    pub hottest_directory: String,
    pub observed_share_percent: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FileSummary {
    pub file_path: String,
    pub last_pid_touching_file: i32,
    pub readers_count: usize,
    pub writers_count: usize,
    pub read_ops: u64,
    pub write_ops: u64,
    pub read_bytes: u64,
    pub write_bytes: u64,
    pub fsync_count: u64,
    pub last_access_timestamp: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DirectorySummary {
    pub directory_path: String,
    pub total_read_bytes: u64,
    pub total_write_bytes: u64,
    pub top_processes: Vec<i32>,
    pub file_count_touched: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DeviceSummary {
    pub device: String,
    pub reads_completed: u64,
    pub writes_completed: u64,
    pub sectors_read: u64,
    pub sectors_written: u64,
    pub time_spent_reading_ms: u64,
    pub time_spent_writing_ms: u64,
    pub current_ios_in_progress: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiagnosticReport {
    pub mode: DiagnosticMode,
    pub session_id: Option<String>,
    pub started_at: DateTime<Utc>,
    pub ended_at: DateTime<Utc>,
    pub duration_seconds: u64,
    pub top_processes: Vec<ProcessSummary>,
    pub top_files: Vec<FileSummary>,
    pub top_directories: Vec<DirectorySummary>,
    pub device_totals: Vec<DeviceSummary>,
    pub unresolved_path_count: u64,
    pub dropped_event_count: u64,
    pub attribution_note: String,
    pub fallback_message: Option<String>,
}

#[derive(Debug, Clone, Default)]
struct ProcIo {
    ppid: i32,
    uid: u32,
    command: String,
    exe_path: String,
    cwd: String,
    rchar: u64,
    wchar: u64,
    syscr: u64,
    syscw: u64,
    read_bytes: u64,
    write_bytes: u64,
    cancelled_write_bytes: u64,
    open_files: Vec<String>,
}

#[derive(Debug, Clone, Default)]
struct ProcAgg {
    pid: i32,
    ppid: i32,
    uid: u32,
    command: String,
    exe_path: String,
    cwd: String,
    logical_read_bytes: u64,
    logical_write_bytes: u64,
    storage_read_bytes: u64,
    storage_write_bytes: u64,
    cancelled_write_bytes: u64,
    read_syscalls: u64,
    write_syscalls: u64,
    open_file_count: usize,
    file_hits: HashMap<String, u64>,
    dir_hits: HashMap<String, u64>,
}

pub struct LiveSamplerState {
    session_id: Option<String>,
    started_at: DateTime<Utc>,
    started: Instant,
    prev: HashMap<i32, ProcIo>,
    start_devices: HashMap<String, Vec<u64>>,
    per_pid: HashMap<i32, ProcAgg>,
    file_map: BTreeMap<String, FileSummary>,
    unresolved_path_count: u64,
}

pub fn run_live_sampler(
    duration: Duration,
    interval: Duration,
    session_id: Option<String>,
) -> Result<DiagnosticReport> {
    run_live_sampler_internal(duration, interval, session_id, None)
}

impl LiveSamplerState {
    pub fn new(session_id: Option<String>) -> Result<Self> {
        Ok(Self {
            session_id,
            started_at: Utc::now(),
            started: Instant::now(),
            prev: snapshot_procfs()?,
            start_devices: diskstats_map(),
            per_pid: HashMap::new(),
            file_map: BTreeMap::new(),
            unresolved_path_count: 0,
        })
    }

    pub fn tick(&mut self, interval: Duration) -> Result<DiagnosticReport> {
        thread::sleep(interval);
        self.sample_once()
    }

    pub fn sample_once(&mut self) -> Result<DiagnosticReport> {
        let current = snapshot_procfs()?;
        apply_proc_delta(
            &self.prev,
            &current,
            &mut self.per_pid,
            &mut self.file_map,
            &mut self.unresolved_path_count,
        );
        self.prev = current;
        Ok(build_sampler_report(
            self.session_id.clone(),
            self.started_at,
            self.started,
            &self.per_pid,
            &self.file_map,
            self.unresolved_path_count,
            self.start_devices.clone(),
        ))
    }
}

pub fn run_live_sampler_until(
    interval: Duration,
    session_id: Option<String>,
    stop_signal: Arc<AtomicBool>,
) -> Result<DiagnosticReport> {
    run_live_sampler_internal(
        Duration::from_secs(u64::MAX / 4),
        interval,
        session_id,
        Some(stop_signal),
    )
}

fn run_live_sampler_internal(
    duration: Duration,
    interval: Duration,
    session_id: Option<String>,
    stop_signal: Option<Arc<AtomicBool>>,
) -> Result<DiagnosticReport> {
    if !Path::new("/proc").exists() {
        return Ok(DiagnosticReport {
            mode: DiagnosticMode::Sample,
            session_id,
            started_at: Utc::now(),
            ended_at: Utc::now(),
            duration_seconds: 0,
            top_processes: vec![],
            top_files: vec![],
            top_directories: vec![],
            device_totals: vec![],
            unresolved_path_count: 0,
            dropped_event_count: 0,
            attribution_note: "procfs is unavailable on this host, so live sampler attribution could not be collected.".to_string(),
            fallback_message: Some(
                "live sampler unavailable because /proc is not present or not readable".to_string(),
            ),
        });
    }
    let mut state = LiveSamplerState::new(session_id)?;
    let mut last_report = build_sampler_report(
        state.session_id.clone(),
        state.started_at,
        state.started,
        &state.per_pid,
        &state.file_map,
        state.unresolved_path_count,
        state.start_devices.clone(),
    );

    while state.started.elapsed() < duration
        && !stop_signal
            .as_ref()
            .map(|signal| signal.load(AtomicOrdering::Relaxed))
            .unwrap_or(false)
    {
        last_report = state.tick(interval)?;
    }
    Ok(last_report)
}

fn apply_proc_delta(
    prev: &HashMap<i32, ProcIo>,
    current: &HashMap<i32, ProcIo>,
    per_pid: &mut HashMap<i32, ProcAgg>,
    file_map: &mut BTreeMap<String, FileSummary>,
    unresolved_path_count: &mut u64,
) {
    for (pid, after) in current {
        let Some(before) = prev.get(pid) else {
            continue;
        };
        let agg = per_pid.entry(*pid).or_insert_with(|| ProcAgg {
            pid: *pid,
            ppid: after.ppid,
            uid: after.uid,
            command: after.command.clone(),
            exe_path: after.exe_path.clone(),
            cwd: after.cwd.clone(),
            ..ProcAgg::default()
        });
        agg.logical_read_bytes = agg
            .logical_read_bytes
            .saturating_add(after.rchar.saturating_sub(before.rchar));
        agg.logical_write_bytes = agg
            .logical_write_bytes
            .saturating_add(after.wchar.saturating_sub(before.wchar));
        agg.storage_read_bytes = agg
            .storage_read_bytes
            .saturating_add(after.read_bytes.saturating_sub(before.read_bytes));
        agg.storage_write_bytes = agg
            .storage_write_bytes
            .saturating_add(after.write_bytes.saturating_sub(before.write_bytes));
        agg.cancelled_write_bytes = agg.cancelled_write_bytes.saturating_add(
            after
                .cancelled_write_bytes
                .saturating_sub(before.cancelled_write_bytes),
        );
        agg.read_syscalls = agg
            .read_syscalls
            .saturating_add(after.syscr.saturating_sub(before.syscr));
        agg.write_syscalls = agg
            .write_syscalls
            .saturating_add(after.syscw.saturating_sub(before.syscw));
        agg.open_file_count = after.open_files.len();
        let mut unique_paths = HashSet::new();
        for path in &after.open_files {
            if unique_paths.insert(path.clone()) {
                *agg.file_hits.entry(path.clone()).or_default() += 1;
                if let Some(parent) = Path::new(path).parent() {
                    *agg.dir_hits
                        .entry(parent.display().to_string())
                        .or_default() += 1;
                } else {
                    *unresolved_path_count = unresolved_path_count.saturating_add(1);
                }
            }
        }

        let share_count = unique_paths.len().max(1) as u64;
        let read_share = after.read_bytes.saturating_sub(before.read_bytes) / share_count;
        let write_share = after.write_bytes.saturating_sub(before.write_bytes) / share_count;
        let read_ops_share = after.syscr.saturating_sub(before.syscr) / share_count;
        let write_ops_share = after.syscw.saturating_sub(before.syscw) / share_count;
        for path in unique_paths {
            let entry = file_map.entry(path.clone()).or_insert_with(|| FileSummary {
                file_path: path.clone(),
                ..FileSummary::default()
            });
            entry.last_pid_touching_file = *pid;
            entry.last_access_timestamp = Utc::now().to_rfc3339();
            if read_share > 0 || read_ops_share > 0 {
                entry.readers_count += 1;
            }
            if write_share > 0 || write_ops_share > 0 {
                entry.writers_count += 1;
            }
            entry.read_bytes = entry.read_bytes.saturating_add(read_share);
            entry.write_bytes = entry.write_bytes.saturating_add(write_share);
            entry.read_ops = entry.read_ops.saturating_add(read_ops_share);
            entry.write_ops = entry.write_ops.saturating_add(write_ops_share);
        }
    }
}

fn build_sampler_report(
    session_id: Option<String>,
    started_at: DateTime<Utc>,
    started: Instant,
    per_pid: &HashMap<i32, ProcAgg>,
    file_map: &BTreeMap<String, FileSummary>,
    unresolved_path_count: u64,
    start_devices: HashMap<String, Vec<u64>>,
) -> DiagnosticReport {
    let elapsed_secs = started.elapsed().as_secs().max(1);
    let total_observed_storage = per_pid
        .values()
        .map(|p| p.storage_read_bytes + p.storage_write_bytes)
        .sum::<u64>() as f64;
    let mut processes = per_pid
        .values()
        .map(|agg| {
            let hottest_file = agg
                .file_hits
                .iter()
                .max_by_key(|(_, hits)| *hits)
                .map(|(path, _)| path.clone())
                .unwrap_or_else(|| "<unresolved>".to_string());
            let hottest_directory = agg
                .dir_hits
                .iter()
                .max_by_key(|(_, hits)| *hits)
                .map(|(path, _)| path.clone())
                .unwrap_or_else(|| "<unresolved>".to_string());
            let observed = (agg.storage_read_bytes + agg.storage_write_bytes) as f64;
            ProcessSummary {
                pid: agg.pid,
                ppid: agg.ppid,
                user: resolve_uid_to_user(agg.uid),
                command: agg.command.clone(),
                exe_path: agg.exe_path.clone(),
                cwd: agg.cwd.clone(),
                logical_read_bytes: agg.logical_read_bytes,
                logical_write_bytes: agg.logical_write_bytes,
                storage_read_bytes: agg.storage_read_bytes,
                storage_write_bytes: agg.storage_write_bytes,
                logical_read_bytes_per_sec: agg.logical_read_bytes as f64 / elapsed_secs as f64,
                logical_write_bytes_per_sec: agg.logical_write_bytes as f64 / elapsed_secs as f64,
                storage_read_bytes_per_sec: agg.storage_read_bytes as f64 / elapsed_secs as f64,
                storage_write_bytes_per_sec: agg.storage_write_bytes as f64 / elapsed_secs as f64,
                cancelled_write_bytes: agg.cancelled_write_bytes,
                read_syscalls: agg.read_syscalls,
                write_syscalls: agg.write_syscalls,
                read_syscalls_per_sec: agg.read_syscalls as f64 / elapsed_secs as f64,
                write_syscalls_per_sec: agg.write_syscalls as f64 / elapsed_secs as f64,
                sync_count: 0,
                open_file_count: agg.open_file_count,
                hottest_file,
                hottest_directory,
                observed_share_percent: if total_observed_storage > 0.0 {
                    observed * 100.0 / total_observed_storage
                } else {
                    0.0
                },
            }
        })
        .collect::<Vec<_>>();
    processes.sort_by(sort_processes);
    processes.truncate(25);

    let mut files = file_map.values().cloned().collect::<Vec<_>>();
    files.sort_by(|a, b| {
        (b.read_bytes + b.write_bytes)
            .cmp(&(a.read_bytes + a.write_bytes))
            .then_with(|| a.file_path.cmp(&b.file_path))
    });
    files.truncate(25);

    let mut directories = summarize_directories(&files);
    directories.sort_by(|a, b| {
        (b.total_read_bytes + b.total_write_bytes)
            .cmp(&(a.total_read_bytes + a.total_write_bytes))
            .then_with(|| a.directory_path.cmp(&b.directory_path))
    });
    directories.truncate(25);

    let devices = summarize_devices(start_devices, diskstats_map());

    DiagnosticReport {
        mode: DiagnosticMode::Sample,
        session_id,
        started_at,
        ended_at: Utc::now(),
        duration_seconds: elapsed_secs,
        top_processes: processes,
        top_files: files,
        top_directories: directories,
        device_totals: devices,
        unresolved_path_count,
        dropped_event_count: 0,
        attribution_note: "Logical syscall bytes (rchar/wchar) and storage-layer bytes (read_bytes/write_bytes) are reported separately. File and directory attribution in sampler mode is best-effort based on observed open descriptors and procfs deltas; it is not exact per-file kernel attribution.".to_string(),
        fallback_message: None,
    }
}

pub fn run_deep_trace(
    duration: Duration,
    session_id: Option<String>,
    fallback_to_sampler: bool,
) -> Result<DiagnosticReport> {
    if !deep_trace_available() {
        if fallback_to_sampler {
            let mut report = run_live_sampler(duration, Duration::from_secs(1), session_id)?;
            report.mode = DiagnosticMode::DeepTrace;
            report.fallback_message = Some(
                "deep trace unavailable; falling back to live sampler because root and tracing facilities are not available".to_string(),
            );
            report.dropped_event_count = 0;
            return Ok(report);
        }
        return Ok(DiagnosticReport {
            mode: DiagnosticMode::DeepTrace,
            session_id,
            started_at: Utc::now(),
            ended_at: Utc::now(),
            duration_seconds: 0,
            top_processes: vec![],
            top_files: vec![],
            top_directories: vec![],
            device_totals: vec![],
            unresolved_path_count: 0,
            dropped_event_count: 0,
            attribution_note: "deep trace requires root and kernel tracing support; when unavailable, the application can fall back to sampler mode".to_string(),
            fallback_message: Some(
                "deep trace unavailable: root privileges and tracing/eBPF support were not detected".to_string(),
            ),
        });
    }

    let mut report = run_live_sampler(duration, Duration::from_secs(1), session_id)?;
    report.mode = DiagnosticMode::DeepTrace;
    report.fallback_message = Some(
        "deep trace prerequisites are present, but this build is using sampler-grade attribution as a graceful fallback because the optional eBPF tracer backend is not bundled.".to_string(),
    );
    report.attribution_note = "This report is a best-effort fallback, not a kernel event trace. Requested/completed bytes, errno attribution, and syscall latency require the optional deep tracer backend.".to_string();
    Ok(report)
}

fn snapshot_procfs() -> Result<HashMap<i32, ProcIo>> {
    let mut map = HashMap::new();
    for entry in fs::read_dir("/proc").context("read /proc")? {
        let entry = entry?;
        let file_name = entry.file_name();
        let Ok(pid) = file_name.to_string_lossy().parse::<i32>() else {
            continue;
        };
        if let Ok(snapshot) = read_pid_snapshot(pid) {
            map.insert(pid, snapshot);
        }
    }
    Ok(map)
}

fn read_pid_snapshot(pid: i32) -> Result<ProcIo> {
    let io_text = fs::read_to_string(format!("/proc/{}/io", pid)).context("read pid io")?;
    let status_text = fs::read_to_string(format!("/proc/{}/status", pid)).unwrap_or_default();
    let mut snapshot = ProcIo {
        command: read_cmdline(pid).unwrap_or_else(|| pid.to_string()),
        exe_path: read_link_string(format!("/proc/{}/exe", pid))
            .unwrap_or_else(|| "<unresolved>".to_string()),
        cwd: read_link_string(format!("/proc/{}/cwd", pid))
            .unwrap_or_else(|| "<unresolved>".to_string()),
        open_files: read_open_files(pid),
        ..ProcIo::default()
    };
    for line in io_text.lines() {
        if let Some((key, value)) = parse_kv(line) {
            match key {
                "rchar" => snapshot.rchar = value,
                "wchar" => snapshot.wchar = value,
                "syscr" => snapshot.syscr = value,
                "syscw" => snapshot.syscw = value,
                "read_bytes" => snapshot.read_bytes = value,
                "write_bytes" => snapshot.write_bytes = value,
                "cancelled_write_bytes" => snapshot.cancelled_write_bytes = value,
                _ => {}
            }
        }
    }
    for line in status_text.lines() {
        if let Some(rest) = line.strip_prefix("Uid:") {
            snapshot.uid = rest
                .split_whitespace()
                .find_map(|v| v.parse::<u32>().ok())
                .unwrap_or(0);
        }
        if let Some(rest) = line.strip_prefix("PPid:") {
            snapshot.ppid = rest.trim().parse::<i32>().unwrap_or(0);
        }
        if let Some(rest) = line.strip_prefix("Name:") {
            if snapshot.command.is_empty() {
                snapshot.command = rest.trim().to_string();
            }
        }
    }
    Ok(snapshot)
}

fn read_open_files(pid: i32) -> Vec<String> {
    let mut paths = Vec::new();
    let dir = format!("/proc/{}/fd", pid);
    let Ok(entries) = fs::read_dir(dir) else {
        return paths;
    };
    for entry in entries.flatten() {
        if let Some(path) = read_link_string(entry.path()) {
            paths.push(path);
        }
    }
    paths
}

fn read_cmdline(pid: i32) -> Option<String> {
    let path = format!("/proc/{}/cmdline", pid);
    let bytes = fs::read(path).ok()?;
    if bytes.is_empty() {
        return None;
    }
    let pieces = bytes
        .split(|b| *b == 0)
        .filter(|part| !part.is_empty())
        .map(|part| String::from_utf8_lossy(part).to_string())
        .collect::<Vec<_>>();
    if pieces.is_empty() {
        None
    } else {
        Some(pieces.join(" "))
    }
}

fn parse_kv(line: &str) -> Option<(&str, u64)> {
    let (key, value) = line.split_once(':')?;
    Some((key.trim(), value.trim().parse::<u64>().ok()?))
}

fn summarize_directories(files: &[FileSummary]) -> Vec<DirectorySummary> {
    let mut map: BTreeMap<String, DirectorySummary> = BTreeMap::new();
    for file in files {
        let parent = Path::new(&file.file_path)
            .parent()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "<unresolved>".to_string());
        let entry = map
            .entry(parent.clone())
            .or_insert_with(|| DirectorySummary {
                directory_path: parent,
                ..DirectorySummary::default()
            });
        entry.total_read_bytes = entry.total_read_bytes.saturating_add(file.read_bytes);
        entry.total_write_bytes = entry.total_write_bytes.saturating_add(file.write_bytes);
        entry.file_count_touched += 1;
        if !entry.top_processes.contains(&file.last_pid_touching_file) {
            entry.top_processes.push(file.last_pid_touching_file);
        }
    }
    map.into_values().collect()
}

fn summarize_devices(
    before: HashMap<String, Vec<u64>>,
    after: HashMap<String, Vec<u64>>,
) -> Vec<DeviceSummary> {
    let mut summaries = Vec::new();
    for (name, values_after) in after {
        let Some(values_before) = before.get(&name) else {
            continue;
        };
        if values_after.len() < 11 || values_before.len() < 11 {
            continue;
        }
        summaries.push(DeviceSummary {
            device: format!("/dev/{}", name),
            reads_completed: values_after[0].saturating_sub(values_before[0]),
            writes_completed: values_after[4].saturating_sub(values_before[4]),
            sectors_read: values_after[2].saturating_sub(values_before[2]),
            sectors_written: values_after[6].saturating_sub(values_before[6]),
            time_spent_reading_ms: values_after[3].saturating_sub(values_before[3]),
            time_spent_writing_ms: values_after[7].saturating_sub(values_before[7]),
            current_ios_in_progress: values_after[8],
        });
    }
    summaries.sort_by(|a, b| {
        (b.sectors_read + b.sectors_written)
            .cmp(&(a.sectors_read + a.sectors_written))
            .then_with(|| a.device.cmp(&b.device))
    });
    summaries.truncate(16);
    summaries
}

fn sort_processes(a: &ProcessSummary, b: &ProcessSummary) -> Ordering {
    let a_total = a.storage_read_bytes_per_sec + a.storage_write_bytes_per_sec;
    let b_total = b.storage_read_bytes_per_sec + b.storage_write_bytes_per_sec;
    b_total
        .partial_cmp(&a_total)
        .unwrap_or(Ordering::Equal)
        .then_with(|| a.pid.cmp(&b.pid))
}

#[cfg(test)]
mod tests {
    use super::{parse_kv, summarize_directories, FileSummary};

    #[test]
    fn parses_proc_io_kv() {
        assert_eq!(parse_kv("rchar: 123"), Some(("rchar", 123)));
    }

    #[test]
    fn groups_files_by_directory() {
        let files = vec![FileSummary {
            file_path: "/tmp/a.bin".to_string(),
            read_bytes: 10,
            write_bytes: 20,
            last_pid_touching_file: 42,
            ..FileSummary::default()
        }];
        let dirs = summarize_directories(&files);
        assert_eq!(dirs[0].directory_path, "/tmp");
        assert_eq!(dirs[0].total_write_bytes, 20);
    }
}
