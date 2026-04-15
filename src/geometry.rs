use crate::system::read_trimmed;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BlockGeometry {
    pub device: PathBuf,
    pub logical_block_size: u64,
    pub physical_block_size: u64,
    pub minimum_io_size: u64,
    pub optimal_io_size: u64,
    pub discard_granularity: u64,
    pub discard_max_bytes: u64,
    pub discard_zeroes_data: Option<bool>,
    pub partition_start_lba: Option<u64>,
    pub alignment_offset: u64,
    pub read_only: bool,
    pub removable: bool,
    pub write_cache: Option<String>,
    pub scheduler: Option<String>,
    pub size_sectors: u64,
    pub size_bytes: u64,
}

pub fn collect_block_geometry(device: &Path) -> Result<Option<BlockGeometry>> {
    let Some(name) = device.file_name().and_then(|value| value.to_str()) else {
        return Ok(None);
    };
    let sys_path = PathBuf::from("/sys/class/block").join(name);
    if !sys_path.exists() {
        return Ok(None);
    }

    let queue_path = queue_source_path(&sys_path)?;
    let logical_block_size = read_u64(queue_path.join("queue/logical_block_size")).unwrap_or(512);
    let physical_block_size =
        read_u64(queue_path.join("queue/physical_block_size")).unwrap_or(logical_block_size);
    let minimum_io_size = read_u64(queue_path.join("queue/minimum_io_size")).unwrap_or(0);
    let optimal_io_size = read_u64(queue_path.join("queue/optimal_io_size")).unwrap_or(0);
    let discard_granularity = read_u64(queue_path.join("queue/discard_granularity")).unwrap_or(0);
    let discard_max_bytes = read_u64(queue_path.join("queue/discard_max_bytes")).unwrap_or(0);
    let discard_zeroes_data = read_bool(queue_path.join("queue/discard_zeroes_data"));
    let partition_start_lba = read_u64(sys_path.join("start"));
    let alignment_offset = read_u64(sys_path.join("alignment_offset")).unwrap_or(0);
    let read_only = read_bool(sys_path.join("ro")).unwrap_or(false);
    let removable = read_bool(queue_path.join("removable"))
        .or_else(|| read_bool(sys_path.join("removable")))
        .unwrap_or(false);
    let write_cache =
        read_trimmed(queue_path.join("queue/write_cache")).map(|value| active_choice(&value));
    let scheduler =
        read_trimmed(queue_path.join("queue/scheduler")).map(|value| active_choice(&value));
    let size_sectors = read_u64(sys_path.join("size")).unwrap_or(0);
    let size_bytes = size_sectors.saturating_mul(logical_block_size.max(1));

    Ok(Some(BlockGeometry {
        device: device.to_path_buf(),
        logical_block_size,
        physical_block_size,
        minimum_io_size,
        optimal_io_size,
        discard_granularity,
        discard_max_bytes,
        discard_zeroes_data,
        partition_start_lba,
        alignment_offset,
        read_only,
        removable,
        write_cache,
        scheduler,
        size_sectors,
        size_bytes,
    }))
}

fn queue_source_path(sys_path: &Path) -> Result<PathBuf> {
    if sys_path.join("queue").exists() {
        return Ok(sys_path.to_path_buf());
    }
    let canonical = fs::canonicalize(sys_path)?;
    if let Some(parent) = canonical.parent() {
        let queue = parent.join("queue");
        if queue.exists() {
            return Ok(parent.to_path_buf());
        }
    }
    Ok(sys_path.to_path_buf())
}

fn read_u64(path: PathBuf) -> Option<u64> {
    read_trimmed(path).and_then(|value| value.parse::<u64>().ok())
}

fn read_bool(path: PathBuf) -> Option<bool> {
    match read_trimmed(path)?.as_str() {
        "0" => Some(false),
        "1" => Some(true),
        _ => None,
    }
}

fn active_choice(raw: &str) -> String {
    raw.split_whitespace()
        .find(|item| item.starts_with('[') && item.ends_with(']'))
        .map(|item| item.trim_matches(['[', ']']).to_string())
        .filter(|item| !item.is_empty())
        .unwrap_or_else(|| raw.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::active_choice;

    #[test]
    fn parses_active_scheduler_choice() {
        assert_eq!(active_choice("none [mq-deadline] kyber"), "mq-deadline");
        assert_eq!(active_choice("write back"), "write back");
    }
}
