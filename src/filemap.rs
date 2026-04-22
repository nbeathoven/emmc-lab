use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[cfg(target_os = "linux")]
use crate::health::read_emmc_health;
#[cfg(target_os = "linux")]
use anyhow::{bail, Context};
#[cfg(target_os = "linux")]
use std::cmp;
#[cfg(target_os = "linux")]
use std::fs::{self, File};
#[cfg(target_os = "linux")]
use std::os::fd::AsRawFd;
#[cfg(target_os = "linux")]
use std::os::unix::fs::OpenOptionsExt;

/// A file extent mapped to physical storage offsets.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilemapExtent {
    pub logical_offset: u64,
    pub physical_offset: u64,
    pub length: u64,
    pub flags: u32,
    pub flag_names: Vec<String>,
}

/// The resolved filesystem mount for a file path.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MountResolution {
    pub mountpoint: PathBuf,
    pub device: PathBuf,
    pub fs_type: String,
}

/// The full file mapping report.
#[derive(Debug, Clone, Serialize, Deserialize)]
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

/// Collect a file-to-LBA mapping using FIEMAP with FIBMAP fallback on Linux.
pub fn collect_file_map(file: &Path, device_override: Option<&Path>) -> Result<FilemapReport> {
    #[cfg(target_os = "linux")]
    {
        collect_file_map_linux(file, device_override)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (file, device_override);
        Err(anyhow!("not supported on this platform"))
    }
}

#[cfg(target_os = "linux")]
fn collect_file_map_linux(file: &Path, device_override: Option<&Path>) -> Result<FilemapReport> {
    let metadata =
        fs::symlink_metadata(file).with_context(|| format!("failed to stat {}", file.display()))?;
    if metadata.file_type().is_symlink() {
        bail!("refusing to FIEMAP a symlink: {}", file.display());
    }

    // Resolve the mounted block device once up front so the report can be tied back
    // to a concrete partition or whole-disk node.
    let mount = resolve_mount_for_file(file)?;
    let device = device_override
        .map(Path::to_path_buf)
        .unwrap_or_else(|| mount.device.clone());
    let file_handle = open_nofollow(file)?;

    // FIEMAP is the normal fast path. FIBMAP is only here for older filesystems and
    // kernels that still reject FIEMAP but can answer block-by-block queries.
    let (extents, method) = match fiemap_extents(&file_handle) {
        Ok(extents) => (extents, "fiemap".to_string()),
        Err(fiemap_err) => match fibmap_extents(&file_handle) {
            Ok(extents) => (extents, "fibmap".to_string()),
            Err(fibmap_err) => {
                return Err(fiemap_err).context(format!(
                    "FIEMAP failed and FIBMAP fallback also failed: {fibmap_err:#}"
                ));
            }
        },
    };
    let sector_size = block_device_sector_size(&device)
        .with_context(|| format!("failed to read sector size for {}", device.display()))?;
    let partition_start_lba = read_partition_start_lba(&device)?;
    let erase_block_size = read_emmc_health(&device, None, None)
        .ok()
        .and_then(|snapshot| snapshot.erase_group_size_bytes);
    let total_extent_bytes = extents.iter().map(|extent| extent.length).sum();
    let extent_count = extents.len();

    Ok(FilemapReport {
        file_path: file.to_path_buf(),
        device,
        sector_size,
        partition_start_lba,
        erase_block_size,
        extents,
        total_extent_bytes,
        extent_count,
        method,
    })
}

#[cfg(target_os = "linux")]
fn open_nofollow(file: &Path) -> Result<File> {
    let handle = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(file)
        .with_context(|| format!("failed to open {}", file.display()))?;
    Ok(handle)
}

/// Read extents via FIEMAP. Linux-only.
#[cfg(target_os = "linux")]
fn fiemap_extents(file: &std::fs::File) -> Result<Vec<FilemapExtent>> {
    let mut extents = Vec::new();
    let mut start = 0_u64;
    loop {
        let mut request = FiemapRequest {
            fm_start: start,
            fm_length: !0_u64,
            fm_flags: 0,
            fm_mapped_extents: 0,
            fm_extent_count: FIEMAP_BATCH as u32,
            fm_reserved: 0,
            fm_extents: [FiemapExtentRaw::default(); FIEMAP_BATCH],
        };
        let rc = unsafe { libc::ioctl(file.as_raw_fd(), FS_IOC_FIEMAP, &mut request) };
        if rc != 0 {
            return Err(std::io::Error::last_os_error()).context("FIEMAP ioctl failed");
        }
        let mapped = request.fm_mapped_extents as usize;
        if mapped == 0 {
            break;
        }

        let mut saw_last = false;
        for raw_extent in request.fm_extents.iter().take(mapped) {
            extents.push(FilemapExtent {
                logical_offset: raw_extent.fe_logical,
                physical_offset: raw_extent.fe_physical,
                length: raw_extent.fe_length,
                flags: raw_extent.fe_flags,
                flag_names: decode_fiemap_flags(raw_extent.fe_flags),
            });
            start = raw_extent.fe_logical.saturating_add(raw_extent.fe_length);
            if raw_extent.fe_flags & FIEMAP_EXTENT_LAST != 0 {
                saw_last = true;
            }
        }
        if saw_last {
            break;
        }
    }
    Ok(extents)
}

/// FIBMAP fallback. Linux-only.
#[cfg(target_os = "linux")]
fn fibmap_extents(file: &std::fs::File) -> Result<Vec<FilemapExtent>> {
    let mut block_size: libc::c_int = 0;
    let rc = unsafe { libc::ioctl(file.as_raw_fd(), FIGETBSZ, &mut block_size) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error()).context("FIGETBSZ ioctl failed");
    }
    if block_size <= 0 {
        bail!("filesystem reported an invalid block size");
    }

    let block_size = block_size as u64;
    let file_len = file
        .metadata()
        .context("failed to read file metadata")?
        .len();
    let block_count = if file_len == 0 {
        0
    } else {
        (file_len + block_size - 1) / block_size
    };
    let mut extents = Vec::new();
    let mut current: Option<FilemapExtent> = None;

    for index in 0..block_count {
        let logical_offset = index.saturating_mul(block_size);
        let chunk_len = cmp::min(block_size, file_len.saturating_sub(logical_offset));
        let mut block = index as libc::c_int;
        let rc = unsafe { libc::ioctl(file.as_raw_fd(), FIBMAP, &mut block) };
        if rc != 0 {
            return Err(std::io::Error::last_os_error()).context("FIBMAP ioctl failed");
        }
        if block == 0 {
            if let Some(extent) = current.take() {
                extents.push(extent);
            }
            continue;
        }
        let physical_offset = (block as u64).saturating_mul(block_size);
        match current.as_mut() {
            Some(extent)
                if extent.logical_offset.saturating_add(extent.length) == logical_offset
                    && extent.physical_offset.saturating_add(extent.length) == physical_offset =>
            {
                extent.length = extent.length.saturating_add(chunk_len);
            }
            Some(_) => {
                extents.push(current.take().expect("current extent is present"));
                current = Some(FilemapExtent {
                    logical_offset,
                    physical_offset,
                    length: chunk_len,
                    flags: 0,
                    flag_names: Vec::new(),
                });
            }
            None => {
                current = Some(FilemapExtent {
                    logical_offset,
                    physical_offset,
                    length: chunk_len,
                    flags: 0,
                    flag_names: Vec::new(),
                });
            }
        }
    }

    if let Some(extent) = current {
        extents.push(extent);
    }
    Ok(extents)
}

/// Decode FIEMAP bit flags into stable human-readable names.
pub fn decode_fiemap_flags(flags: u32) -> Vec<String> {
    let candidates = [
        (FIEMAP_EXTENT_LAST, "last"),
        (FIEMAP_EXTENT_UNKNOWN, "unknown"),
        (FIEMAP_EXTENT_DELALLOC, "delalloc"),
        (FIEMAP_EXTENT_ENCODED, "encoded"),
        (FIEMAP_EXTENT_DATA_ENCRYPTED, "encrypted"),
        (FIEMAP_EXTENT_NOT_ALIGNED, "not-aligned"),
        (FIEMAP_EXTENT_DATA_INLINE, "inline"),
        (FIEMAP_EXTENT_DATA_TAIL, "tail-packed"),
        (FIEMAP_EXTENT_UNWRITTEN, "unwritten"),
        (FIEMAP_EXTENT_MERGED, "merged"),
        (FIEMAP_EXTENT_SHARED, "shared"),
    ];
    candidates
        .iter()
        .filter_map(|(bit, name)| (flags & *bit != 0).then_some((*name).to_string()))
        .collect()
}

/// Resolve the longest matching mount for a file by parsing `/proc/self/mountinfo`.
#[cfg(target_os = "linux")]
fn resolve_mount_for_file(file: &Path) -> Result<MountResolution> {
    let canonical =
        fs::canonicalize(file).with_context(|| format!("failed to resolve {}", file.display()))?;
    let text = fs::read_to_string("/proc/self/mountinfo")
        .context("failed to read /proc/self/mountinfo")?;
    resolve_mount_for_file_from_text(&canonical, &text)
}

#[allow(dead_code)]
fn resolve_mount_for_file_from_text(file: &Path, mountinfo: &str) -> Result<MountResolution> {
    let canonical = file.to_string_lossy();
    let mut best: Option<MountResolution> = None;
    for line in mountinfo.lines() {
        let Some((left, right)) = line.split_once(" - ") else {
            continue;
        };
        let left_parts: Vec<&str> = left.split_whitespace().collect();
        let right_parts: Vec<&str> = right.split_whitespace().collect();
        if left_parts.len() < 5 || right_parts.len() < 2 {
            continue;
        }
        let mountpoint = PathBuf::from(unescape_mount_field(left_parts[4]));
        let mountpoint_str = mountpoint.to_string_lossy();
        if !path_matches_mount(&canonical, &mountpoint_str) {
            continue;
        }
        let candidate = MountResolution {
            mountpoint: mountpoint.clone(),
            device: PathBuf::from(unescape_mount_field(right_parts[1])),
            fs_type: right_parts[0].to_string(),
        };
        let better = best
            .as_ref()
            .map(|current| {
                mountpoint.components().count() > current.mountpoint.components().count()
            })
            .unwrap_or(true);
        if better {
            best = Some(candidate);
        }
    }
    best.ok_or_else(|| anyhow!("failed to resolve mount for {}", file.display()))
}

#[allow(dead_code)]
fn path_matches_mount(path: &str, mountpoint: &str) -> bool {
    if mountpoint == "/" {
        return path.starts_with('/');
    }
    path == mountpoint
        || path
            .strip_prefix(mountpoint)
            .map(|rest| rest.starts_with('/'))
            .unwrap_or(false)
}

#[allow(dead_code)]
fn unescape_mount_field(value: &str) -> String {
    value
        .replace("\\040", " ")
        .replace("\\011", "\t")
        .replace("\\012", "\n")
        .replace("\\134", "\\")
}

/// Read the device logical sector size via BLKSSZGET.
#[cfg(target_os = "linux")]
fn block_device_sector_size(device: &Path) -> Result<u64> {
    if let Some(name) = device
        .file_name()
        .map(|value| value.to_string_lossy().to_string())
    {
        // Partition nodes often inherit queue settings from the parent disk in sysfs.
        // Read sysfs first so ordinary users do not need raw-device open permission
        // just to compute LBAs for a file report.
        for candidate in [Some(name.clone()), parent_block_name(&name)] {
            let Some(candidate) = candidate else {
                continue;
            };
            let sysfs_path = PathBuf::from("/sys/class/block")
                .join(candidate)
                .join("queue/logical_block_size");
            if let Ok(value) = fs::read_to_string(&sysfs_path) {
                if let Ok(parsed) = value.trim().parse::<u64>() {
                    if parsed > 0 {
                        return Ok(parsed);
                    }
                }
            }
        }
    }
    let file =
        File::open(device).with_context(|| format!("failed to open {}", device.display()))?;
    let mut size: libc::c_int = 0;
    let rc = unsafe { libc::ioctl(file.as_raw_fd(), libc::BLKSSZGET, &mut size) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error()).context("BLKSSZGET ioctl failed");
    }
    if size <= 0 {
        bail!("device returned invalid logical sector size");
    }
    Ok(size as u64)
}

#[cfg(target_os = "linux")]
fn parent_block_name(name: &str) -> Option<String> {
    let index = name.rfind('p')?;
    name[index + 1..]
        .chars()
        .all(|ch| ch.is_ascii_digit())
        .then(|| name[..index].to_string())
}

/// Read a partition's start sector from sysfs. Returns `None` for whole-disk devices.
#[cfg(target_os = "linux")]
fn read_partition_start_lba(device: &Path) -> Result<Option<u64>> {
    let Some(name) = device
        .file_name()
        .map(|value| value.to_string_lossy().to_string())
    else {
        return Ok(None);
    };
    let start_path = PathBuf::from("/sys/class/block").join(&name).join("start");
    match fs::read_to_string(&start_path) {
        Ok(value) => Ok(Some(value.trim().parse::<u64>().with_context(|| {
            format!("invalid LBA start in {}", start_path.display())
        })?)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err).with_context(|| format!("failed to read {}", start_path.display())),
    }
}

/// Render the mapping as CSV with one row per extent.
pub fn filemap_report_to_csv(report: &FilemapReport) -> String {
    let mut csv =
        String::from("logical_offset,physical_lba,length,flags,flag_names,erase_aligned\n");
    for extent in &report.extents {
        let physical_lba = extent.physical_offset / report.sector_size.max(1);
        let erase_aligned = report
            .erase_block_size
            .map(|size| size != 0 && extent.physical_offset % size == 0)
            .unwrap_or(false);
        csv.push_str(&format!(
            "{},{},{},{},\"{}\",{}\n",
            extent.logical_offset,
            physical_lba,
            extent.length,
            extent.flags,
            extent.flag_names.join("|"),
            erase_aligned
        ));
    }
    csv
}

#[cfg(target_os = "linux")]
const FIEMAP_BATCH: usize = 256;
pub(crate) const FIEMAP_EXTENT_LAST: u32 = 0x0000_0001;
pub(crate) const FIEMAP_EXTENT_UNKNOWN: u32 = 0x0000_0002;
pub(crate) const FIEMAP_EXTENT_DELALLOC: u32 = 0x0000_0004;
pub(crate) const FIEMAP_EXTENT_ENCODED: u32 = 0x0000_0008;
pub(crate) const FIEMAP_EXTENT_DATA_ENCRYPTED: u32 = 0x0000_0080;
pub(crate) const FIEMAP_EXTENT_NOT_ALIGNED: u32 = 0x0000_0100;
pub(crate) const FIEMAP_EXTENT_DATA_INLINE: u32 = 0x0000_0200;
pub(crate) const FIEMAP_EXTENT_DATA_TAIL: u32 = 0x0000_0400;
pub(crate) const FIEMAP_EXTENT_UNWRITTEN: u32 = 0x0000_0800;
pub(crate) const FIEMAP_EXTENT_MERGED: u32 = 0x0000_1000;
pub(crate) const FIEMAP_EXTENT_SHARED: u32 = 0x0000_2000;

#[cfg(target_os = "linux")]
const IOC_NRBITS: u32 = 8;
#[cfg(target_os = "linux")]
const IOC_TYPEBITS: u32 = 8;
#[cfg(target_os = "linux")]
const IOC_SIZEBITS: u32 = 14;
#[cfg(target_os = "linux")]
const IOC_NRSHIFT: u32 = 0;
#[cfg(target_os = "linux")]
const IOC_TYPESHIFT: u32 = IOC_NRSHIFT + IOC_NRBITS;
#[cfg(target_os = "linux")]
const IOC_SIZESHIFT: u32 = IOC_TYPESHIFT + IOC_TYPEBITS;
#[cfg(target_os = "linux")]
const IOC_DIRSHIFT: u32 = IOC_SIZESHIFT + IOC_SIZEBITS;

#[cfg(target_os = "linux")]
const fn ioc(dir: u32, ty: u32, nr: u32, size: u32) -> libc::c_ulong {
    ((dir << IOC_DIRSHIFT) | (ty << IOC_TYPESHIFT) | (nr << IOC_NRSHIFT) | (size << IOC_SIZESHIFT))
        as libc::c_ulong
}

#[cfg(target_os = "linux")]
#[repr(C)]
struct FiemapRequest {
    fm_start: u64,
    fm_length: u64,
    fm_flags: u32,
    fm_mapped_extents: u32,
    fm_extent_count: u32,
    fm_reserved: u32,
    fm_extents: [FiemapExtentRaw; FIEMAP_BATCH],
}

#[cfg(target_os = "linux")]
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct FiemapExtentRaw {
    fe_logical: u64,
    fe_physical: u64,
    fe_length: u64,
    fe_reserved64: [u64; 2],
    fe_flags: u32,
    fe_reserved: [u32; 3],
}

#[cfg(target_os = "linux")]
const FS_IOC_FIEMAP: libc::c_ulong = 0xC020_660B;
#[cfg(target_os = "linux")]
const FIBMAP: libc::c_ulong = ioc(0, 0, 1, 0);
#[cfg(target_os = "linux")]
const FIGETBSZ: libc::c_ulong = ioc(0, 0, 2, 0);

#[cfg(test)]
mod tests {
    use super::{
        decode_fiemap_flags, resolve_mount_for_file_from_text, FIEMAP_EXTENT_DATA_ENCRYPTED,
        FIEMAP_EXTENT_DATA_INLINE, FIEMAP_EXTENT_DATA_TAIL, FIEMAP_EXTENT_DELALLOC,
        FIEMAP_EXTENT_ENCODED, FIEMAP_EXTENT_LAST, FIEMAP_EXTENT_MERGED, FIEMAP_EXTENT_NOT_ALIGNED,
        FIEMAP_EXTENT_SHARED, FIEMAP_EXTENT_UNKNOWN, FIEMAP_EXTENT_UNWRITTEN,
    };
    use std::path::{Path, PathBuf};

    #[test]
    fn decodes_all_named_fiemap_flags() {
        let flags = FIEMAP_EXTENT_LAST
            | FIEMAP_EXTENT_UNKNOWN
            | FIEMAP_EXTENT_DELALLOC
            | FIEMAP_EXTENT_ENCODED
            | FIEMAP_EXTENT_DATA_ENCRYPTED
            | FIEMAP_EXTENT_NOT_ALIGNED
            | FIEMAP_EXTENT_DATA_INLINE
            | FIEMAP_EXTENT_DATA_TAIL
            | FIEMAP_EXTENT_UNWRITTEN
            | FIEMAP_EXTENT_MERGED
            | FIEMAP_EXTENT_SHARED;
        let names = decode_fiemap_flags(flags);
        assert_eq!(
            names,
            vec![
                "last",
                "unknown",
                "delalloc",
                "encoded",
                "encrypted",
                "not-aligned",
                "inline",
                "tail-packed",
                "unwritten",
                "merged",
                "shared",
            ]
        );
    }

    #[test]
    fn resolves_mount_from_mock_mountinfo() {
        let mountinfo = r#"21 31 179:1 / /boot rw,relatime - vfat /dev/mmcblk0p1 rw
22 31 179:2 / / rw,relatime - ext4 /dev/mmcblk0p2 rw
73 22 0:44 /projects /home/nima/Projects rw,relatime - ext4 /dev/sda1 rw"#;
        let resolved = resolve_mount_for_file_from_text(
            Path::new("/home/nima/Projects/emmc-lab/data.bin"),
            mountinfo,
        )
        .expect("mount should resolve");
        assert_eq!(resolved.mountpoint, PathBuf::from("/home/nima/Projects"));
        assert_eq!(resolved.device, PathBuf::from("/dev/sda1"));
        assert_eq!(resolved.fs_type, "ext4");
    }
}
