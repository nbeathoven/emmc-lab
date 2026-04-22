use crate::system::{DoctorCheck, DoctorStatus};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::ops::Range;
use std::path::PathBuf;

#[cfg(target_os = "linux")]
use crate::filemap::collect_file_map;
#[cfg(target_os = "linux")]
use crate::system::command_exists;
#[cfg(not(target_os = "linux"))]
use anyhow::anyhow;
#[cfg(target_os = "linux")]
use anyhow::{bail, Context};
#[cfg(target_os = "linux")]
use std::path::Path;
#[cfg(target_os = "linux")]
use std::process::Command;

/// Options for CLI-backed LBA trace capture.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LbaTraceOptions {
    pub device: String,
    pub duration_secs: u64,
    pub lba_filter: Option<Range<u64>>,
    pub map_files: Vec<PathBuf>,
}

/// The broad storage operation class inferred from blkparse output.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum IoDirection {
    Read,
    Write,
    Discard,
    Flush,
    Other,
}

/// One traced block-layer event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceEvent {
    pub timestamp_ns: u64,
    #[serde(default)]
    pub pid: Option<u32>,
    pub lba: u64,
    pub length_sectors: u64,
    pub direction: IoDirection,
    pub action: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LbaHeatmapBucket {
    pub start_lba: u64,
    pub end_lba: u64,
    pub read_count: u64,
    pub write_count: u64,
    pub discard_count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct KernelIoBucket {
    pub source: String,
    pub read_events: u64,
    pub write_events: u64,
    pub sectors_read: u64,
    pub sectors_written: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TraceEventSummary {
    pub total_events: u64,
    pub read_events: u64,
    pub write_events: u64,
    pub discard_events: u64,
    pub flush_events: u64,
    pub lba_heatmap: Vec<LbaHeatmapBucket>,
    pub events_per_second: Vec<u64>,
    pub peak_iops_read: f64,
    pub peak_iops_write: f64,
    pub attributed_events: u64,
    pub unattributed_events: u64,
    pub kernel_io_estimate: u64,
    pub kernel_buckets: Vec<KernelIoBucket>,
}

/// A sorted file-ownership range derived from FIEMAP/FIBMAP data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LbaOwnerRange {
    pub start_lba: u64,
    pub end_lba: u64,
    pub file: PathBuf,
    pub offset_in_file: u64,
}

/// A matched file and offset for an observed LBA.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MatchedFile {
    pub file: PathBuf,
    pub offset_in_file: u64,
}

/// The full captured trace report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LbaTraceReport {
    pub device: String,
    pub duration_secs: u64,
    pub event_count: usize,
    pub events: Vec<TraceEvent>,
    pub summary: TraceEventSummary,
    pub owner_index: Vec<LbaOwnerRange>,
    pub method: String,
}

/// Capture a block trace by shelling out to `blktrace` and `blkparse`.
pub fn capture_lba_trace(opts: LbaTraceOptions) -> Result<LbaTraceReport> {
    #[cfg(target_os = "linux")]
    {
        capture_lba_trace_linux(opts)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = opts;
        Err(anyhow!("not supported on this platform"))
    }
}

#[cfg(target_os = "linux")]
fn capture_lba_trace_linux(opts: LbaTraceOptions) -> Result<LbaTraceReport> {
    let check = blktrace_debugfs_available(&opts.device)?;
    if check.status != DoctorStatus::Pass {
        bail!("{}", check.detail);
    }
    if !command_exists("blkparse") {
        bail!("blkparse is not installed");
    }

    // Build the ownership index once before capture starts so each traced LBA can be
    // matched against the current file layout without re-running FIEMAP per event.
    let owner_index = build_lba_owner_index(&opts.map_files)?;
    let device_quoted = shell_single_quote(&opts.device);
    let duration = opts.duration_secs.max(1);

    // This is deliberately a CLI-backed first pass. The format string keeps the output
    // small and stable enough to parse without pretending we already have a relay reader.
    let command = format!(
        "blktrace -d {device_quoted} -w {duration} -o - 2>/dev/null | blkparse -q -i - -f '%T %t %p %a %d %S %n\\n'"
    );
    let output = Command::new("sh")
        .arg("-c")
        .arg(&command)
        .output()
        .context("failed to run blktrace/blkparse pipeline")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("blktrace pipeline failed: {}", stderr.trim());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut events = Vec::new();
    for line in stdout.lines() {
        let Some(event) = parse_trace_line(line) else {
            continue;
        };
        if let Some(filter) = &opts.lba_filter {
            if event.lba < filter.start || event.lba >= filter.end {
                continue;
            }
        }
        events.push(event);
    }

    let summary = summarize_trace_events(&events, duration);

    Ok(LbaTraceReport {
        device: opts.device,
        duration_secs: duration,
        event_count: events.len(),
        events,
        summary,
        owner_index,
        method: "blktrace-cli".to_string(),
    })
}

#[cfg(target_os = "linux")]
fn parse_trace_line(line: &str) -> Option<TraceEvent> {
    let mut parts = line.split_whitespace();
    let seconds = parts.next()?.parse::<u64>().ok()?;
    let nanos = parts.next()?.parse::<u64>().ok()?;
    let pid = parts.next()?.parse::<u32>().ok();
    let action_name = parts.next()?;
    if action_name != "D" {
        return None;
    }
    let rwbs = parts.next()?;
    let lba = parts.next()?.parse::<u64>().ok()?;
    let length_sectors = parts.next()?.parse::<u64>().ok()?;
    let action = action_mask_from_rwbs(rwbs);
    Some(TraceEvent {
        timestamp_ns: seconds.saturating_mul(1_000_000_000).saturating_add(nanos),
        pid,
        lba,
        length_sectors,
        direction: decode_blk_action(action),
        action,
    })
}

#[cfg(target_os = "linux")]
fn summarize_trace_events(events: &[TraceEvent], duration_secs: u64) -> TraceEventSummary {
    let mut summary = TraceEventSummary::default();
    summary.total_events = events.len() as u64;
    let max_lba = events
        .iter()
        .map(|event| event.lba.saturating_add(event.length_sectors))
        .max()
        .unwrap_or(0);
    let bucket_size = (max_lba / 16).max(1);
    summary.lba_heatmap = (0..16)
        .map(|index| LbaHeatmapBucket {
            start_lba: index * bucket_size,
            end_lba: ((index + 1) * bucket_size).saturating_sub(1),
            ..LbaHeatmapBucket::default()
        })
        .collect();
    summary.events_per_second = vec![0; duration_secs.max(1) as usize];
    let mut kernel = KernelIoBucket {
        source: "unknown-kernel".to_string(),
        ..KernelIoBucket::default()
    };
    let mut read_peak = 0_u64;
    let mut write_peak = 0_u64;
    let mut read_per_sec = vec![0_u64; duration_secs.max(1) as usize];
    let mut write_per_sec = vec![0_u64; duration_secs.max(1) as usize];
    for event in events {
        let sec = (event.timestamp_ns / 1_000_000_000)
            .min(summary.events_per_second.len().saturating_sub(1) as u64)
            as usize;
        summary.events_per_second[sec] += 1;
        let bucket_index =
            ((event.lba / bucket_size) as usize).min(summary.lba_heatmap.len().saturating_sub(1));
        match event.direction {
            IoDirection::Read => {
                summary.read_events += 1;
                summary.lba_heatmap[bucket_index].read_count += 1;
                read_per_sec[sec] += 1;
            }
            IoDirection::Write => {
                summary.write_events += 1;
                summary.lba_heatmap[bucket_index].write_count += 1;
                write_per_sec[sec] += 1;
            }
            IoDirection::Discard => {
                summary.discard_events += 1;
                summary.lba_heatmap[bucket_index].discard_count += 1;
            }
            IoDirection::Flush => summary.flush_events += 1,
            IoDirection::Other => {}
        }
        if event.pid.unwrap_or(0) == 0 {
            summary.kernel_io_estimate += 1;
            summary.unattributed_events += 1;
            match event.direction {
                IoDirection::Read => {
                    kernel.read_events += 1;
                    kernel.sectors_read += event.length_sectors;
                }
                IoDirection::Write => {
                    kernel.write_events += 1;
                    kernel.sectors_written += event.length_sectors;
                }
                _ => {}
            }
        } else {
            summary.attributed_events += 1;
        }
    }
    if kernel.read_events != 0 || kernel.write_events != 0 {
        summary.kernel_buckets.push(kernel);
    }
    for value in read_per_sec {
        read_peak = read_peak.max(value);
    }
    for value in write_per_sec {
        write_peak = write_peak.max(value);
    }
    summary.peak_iops_read = read_peak as f64;
    summary.peak_iops_write = write_peak as f64;
    summary
}

#[cfg(target_os = "linux")]
fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

#[cfg(target_os = "linux")]
fn action_mask_from_rwbs(rwbs: &str) -> u32 {
    let mut mask = 0;
    if rwbs.contains('R') {
        mask |= ACTION_READ;
    }
    if rwbs.contains('W') {
        mask |= ACTION_WRITE;
    }
    if rwbs.contains('D') {
        mask |= ACTION_DISCARD;
    }
    if rwbs.contains('F') {
        mask |= ACTION_FLUSH;
    }
    if mask == 0 {
        ACTION_OTHER
    } else {
        mask
    }
}

/// Check whether debugfs and blktrace prerequisites are present for a device.
pub fn blktrace_debugfs_available(device: &str) -> Result<DoctorCheck> {
    #[cfg(target_os = "linux")]
    {
        let device_name = Path::new(device)
            .file_name()
            .map(|value| value.to_string_lossy().to_string())
            .unwrap_or_else(|| device.to_string());
        let debug_root = Path::new("/sys/kernel/debug/block");
        let has_blktrace = command_exists("blktrace");
        let has_blkparse = command_exists("blkparse");
        let detail = if !has_blktrace {
            "install blktrace to capture block-layer events".to_string()
        } else if !has_blkparse {
            "install blkparse to decode blktrace output".to_string()
        } else if !debug_root.exists() {
            "mount debugfs at /sys/kernel/debug before running lba-trace".to_string()
        } else if !debug_root.join(&device_name).exists() {
            format!(
                "{} is not exposed under /sys/kernel/debug/block; kernel tracing may be disabled",
                device_name
            )
        } else {
            format!("blktrace CLI capture is available for {device_name}")
        };
        Ok(DoctorCheck {
            name: "blktrace_debugfs".to_string(),
            status: if has_blktrace && has_blkparse && debug_root.join(&device_name).exists() {
                DoctorStatus::Pass
            } else {
                DoctorStatus::Warn
            },
            detail,
        })
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = device;
        Ok(DoctorCheck {
            name: "blktrace_debugfs".to_string(),
            status: DoctorStatus::Fail,
            detail: "lba-trace is Linux-only".to_string(),
        })
    }
}

#[allow(dead_code)]
/// Convert a simplified action mask into an I/O direction.
fn decode_blk_action(action: u32) -> IoDirection {
    if action & ACTION_DISCARD != 0 {
        IoDirection::Discard
    } else if action & ACTION_FLUSH != 0 {
        IoDirection::Flush
    } else if action & ACTION_WRITE != 0 {
        IoDirection::Write
    } else if action & ACTION_READ != 0 {
        IoDirection::Read
    } else {
        IoDirection::Other
    }
}

/// Build a sorted owner index from file maps.
pub fn build_lba_owner_index(files: &[PathBuf]) -> Result<Vec<LbaOwnerRange>> {
    #[cfg(target_os = "linux")]
    {
        let mut ranges = Vec::new();
        for file in files {
            let report = collect_file_map(file, None)?;
            for extent in report.extents {
                let start_lba = extent.physical_offset / report.sector_size
                    + report.partition_start_lba.unwrap_or(0);
                let sector_count = extent.length / report.sector_size.max(1);
                if sector_count == 0 {
                    continue;
                }
                ranges.push(LbaOwnerRange {
                    start_lba,
                    end_lba: start_lba.saturating_add(sector_count),
                    file: file.clone(),
                    offset_in_file: extent.logical_offset,
                });
            }
        }
        ranges.sort_by_key(|range| range.start_lba);
        Ok(ranges)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = files;
        Err(anyhow!("not supported on this platform"))
    }
}

/// Match an LBA to the owning file range using binary search.
pub fn match_lba_to_file(lba: u64, index: &[LbaOwnerRange]) -> Option<MatchedFile> {
    let found = index.binary_search_by(|range| {
        if lba < range.start_lba {
            std::cmp::Ordering::Greater
        } else if lba >= range.end_lba {
            std::cmp::Ordering::Less
        } else {
            std::cmp::Ordering::Equal
        }
    });
    let position = found.ok()?;
    let range = &index[position];
    Some(MatchedFile {
        file: range.file.clone(),
        offset_in_file: range
            .offset_in_file
            .saturating_add((lba.saturating_sub(range.start_lba)).saturating_mul(512)),
    })
}

#[allow(dead_code)]
const ACTION_READ: u32 = 1 << 0;
#[allow(dead_code)]
const ACTION_WRITE: u32 = 1 << 1;
#[allow(dead_code)]
const ACTION_DISCARD: u32 = 1 << 2;
#[allow(dead_code)]
const ACTION_FLUSH: u32 = 1 << 3;
#[allow(dead_code)]
const ACTION_OTHER: u32 = 1 << 4;

#[cfg(test)]
mod tests {
    use super::{
        decode_blk_action, match_lba_to_file, IoDirection, LbaOwnerRange, ACTION_DISCARD,
        ACTION_FLUSH, ACTION_READ, ACTION_WRITE,
    };
    use std::path::PathBuf;

    #[test]
    fn matches_lba_across_boundaries() {
        let index = vec![
            LbaOwnerRange {
                start_lba: 10,
                end_lba: 20,
                file: PathBuf::from("/tmp/a"),
                offset_in_file: 0,
            },
            LbaOwnerRange {
                start_lba: 30,
                end_lba: 40,
                file: PathBuf::from("/tmp/b"),
                offset_in_file: 4096,
            },
            LbaOwnerRange {
                start_lba: 50,
                end_lba: 60,
                file: PathBuf::from("/tmp/c"),
                offset_in_file: 8192,
            },
        ];
        assert_eq!(
            match_lba_to_file(10, &index).map(|hit| hit.file),
            Some(PathBuf::from("/tmp/a"))
        );
        assert_eq!(
            match_lba_to_file(39, &index).map(|hit| hit.file),
            Some(PathBuf::from("/tmp/b"))
        );
        assert!(match_lba_to_file(25, &index).is_none());
        assert!(match_lba_to_file(60, &index).is_none());
    }

    #[test]
    fn decodes_known_action_masks() {
        assert_eq!(decode_blk_action(ACTION_READ), IoDirection::Read);
        assert_eq!(decode_blk_action(ACTION_WRITE), IoDirection::Write);
        assert_eq!(decode_blk_action(ACTION_DISCARD), IoDirection::Discard);
        assert_eq!(decode_blk_action(ACTION_FLUSH), IoDirection::Flush);
    }
}
