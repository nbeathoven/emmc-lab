use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

/// RAII guard that unlocks a boot partition for the duration of its lifetime
/// and re-locks it on drop. Locking is always restored — even on panic.
pub struct BootPartitionGuard {
    device_name: String,
    was_readonly: bool,
}

impl BootPartitionGuard {
    /// Read /sys/class/block/{name}/force_ro.
    /// If 1, write "0" to unlock. Record original state for Drop.
    /// If already 0, no-op (was_readonly = false, nothing to restore).
    pub fn unlock(device: &Path) -> Result<Self> {
        let device_name = device
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .context("device path is missing a basename")?;
        let force_ro_path = Self::force_ro_path(&device_name);
        let was_readonly = fs::read_to_string(&force_ro_path)
            .with_context(|| format!("failed to read {}", force_ro_path.display()))?
            .trim()
            == "1";
        if was_readonly {
            fs::write(&force_ro_path, "0\n")
                .with_context(|| format!("failed to unlock {}", force_ro_path.display()))?;
        }
        Ok(Self {
            device_name,
            was_readonly,
        })
    }

    fn force_ro_path(device_name: &str) -> PathBuf {
        PathBuf::from(format!("/sys/class/block/{device_name}/force_ro"))
    }
}

impl Drop for BootPartitionGuard {
    fn drop(&mut self) {
        if self.was_readonly {
            let path = Self::force_ro_path(&self.device_name);
            let _ = fs::write(path, "1\n");
        }
    }
}

/// Returns true if the path's basename matches mmcblk{N}boot{M}.
pub fn is_boot_partition(device: &Path) -> bool {
    let Some(name) = device.file_name().map(|name| name.to_string_lossy()) else {
        return false;
    };
    let Some(rest) = name.strip_prefix("mmcblk") else {
        return false;
    };
    let Some((parent_idx, boot_idx)) = rest.split_once("boot") else {
        return false;
    };
    !parent_idx.is_empty()
        && !boot_idx.is_empty()
        && parent_idx.chars().all(|ch| ch.is_ascii_digit())
        && boot_idx.chars().all(|ch| ch.is_ascii_digit())
}

/// Given a parent device name (e.g. "mmcblk0"), return the two boot partition
/// paths if they exist under /sys/class/block/.
pub fn list_boot_partitions(parent_name: &str) -> Vec<PathBuf> {
    (0..=1)
        .map(|idx| format!("{parent_name}boot{idx}"))
        .filter(|name| Path::new("/sys/class/block").join(name).exists())
        .map(|name| PathBuf::from("/dev").join(name))
        .collect()
}

/// Size in bytes: read /sys/class/block/{name}/size (sectors) × 512.
pub fn boot_partition_size_bytes(device: &Path) -> Option<u64> {
    let name = device.file_name()?.to_string_lossy();
    fs::read_to_string(
        Path::new("/sys/class/block")
            .join(name.as_ref())
            .join("size"),
    )
    .ok()?
    .trim()
    .parse::<u64>()
    .ok()
    .map(|sectors| sectors.saturating_mul(512))
}

#[cfg(test)]
mod tests {
    use super::is_boot_partition;
    use std::path::Path;

    #[test]
    fn test_is_boot_partition() {
        assert!(is_boot_partition(Path::new("/dev/mmcblk0boot0")));
        assert!(is_boot_partition(Path::new("/dev/mmcblk1boot1")));
        assert!(!is_boot_partition(Path::new("/dev/mmcblk0")));
        assert!(!is_boot_partition(Path::new("/dev/mmcblk0p1")));
        assert!(!is_boot_partition(Path::new("/dev/sda")));
    }
}
