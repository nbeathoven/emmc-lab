use crate::geometry::{collect_block_geometry, BlockGeometry};
use crate::system::{command_exists, list_devices};
use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::fs::OpenOptions;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::Instant;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct DiscardCapability {
    pub device: PathBuf,
    pub discard_supported: bool,
    pub discard_granularity: u64,
    pub discard_max_bytes: u64,
    pub blkdiscard_available: bool,
    pub fstrim_available: bool,
    pub blkdiscard_ioctl_viable: bool,
    pub probe_result: Option<DiscardProbeResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct DiscardProbeResult {
    pub probe_offset: u64,
    pub probe_length: u64,
    pub success: bool,
    pub duration_us: u64,
    pub error: Option<String>,
}

pub fn detect_discard(device: &Path) -> Result<DiscardCapability> {
    let geometry = collect_block_geometry(device)?
        .ok_or_else(|| anyhow!("no block geometry available for {}", device.display()))?;
    Ok(detect_discard_from_geometry(device, &geometry))
}

pub fn detect_discard_from_geometry(device: &Path, geometry: &BlockGeometry) -> DiscardCapability {
    let ioctl_viable = OpenOptions::new()
        .read(true)
        .write(true)
        .open(device)
        .is_ok();
    DiscardCapability {
        device: device.to_path_buf(),
        discard_supported: geometry.discard_granularity > 0,
        discard_granularity: geometry.discard_granularity,
        discard_max_bytes: geometry.discard_max_bytes,
        blkdiscard_available: command_exists("blkdiscard"),
        fstrim_available: command_exists("fstrim"),
        blkdiscard_ioctl_viable: ioctl_viable,
        probe_result: None,
    }
}

pub fn probe_discard(
    device: &Path,
    offset: u64,
    length: u64,
    destructive_confirmation: bool,
) -> Result<DiscardProbeResult> {
    let capability = detect_discard(device)?;
    validate_discard_request(
        device,
        &capability,
        offset,
        length,
        destructive_confirmation,
    )?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(device)
        .with_context(|| format!("failed to open {}", device.display()))?;
    let range = [offset, length];
    let started = Instant::now();
    let rc = unsafe { libc::ioctl(file.as_raw_fd(), BLKDISCARD, &range) };
    if rc != 0 {
        let error = std::io::Error::last_os_error();
        return Ok(DiscardProbeResult {
            probe_offset: offset,
            probe_length: length,
            success: false,
            duration_us: started.elapsed().as_micros() as u64,
            error: Some(error.to_string()),
        });
    }
    Ok(DiscardProbeResult {
        probe_offset: offset,
        probe_length: length,
        success: true,
        duration_us: started.elapsed().as_micros() as u64,
        error: None,
    })
}

pub fn run_discard_test(
    device: &Path,
    ranges: &[(u64, u64)],
    destructive_confirmation: bool,
) -> Result<Vec<DiscardProbeResult>> {
    let mut results = Vec::with_capacity(ranges.len());
    for (offset, length) in ranges {
        results.push(probe_discard(
            device,
            *offset,
            *length,
            destructive_confirmation,
        )?);
    }
    Ok(results)
}

fn validate_discard_request(
    device: &Path,
    capability: &DiscardCapability,
    offset: u64,
    length: u64,
    destructive_confirmation: bool,
) -> Result<()> {
    if !destructive_confirmation {
        bail!("discard requires --i-understand-this-will-discard-data");
    }
    if !capability.discard_supported {
        bail!("device does not advertise discard support");
    }
    if capability.discard_granularity == 0 {
        bail!("discard granularity is zero");
    }
    if offset % capability.discard_granularity != 0 || length % capability.discard_granularity != 0
    {
        bail!(
            "offset and length must align to discard granularity {}",
            capability.discard_granularity
        );
    }
    if capability.discard_max_bytes != 0 && length > capability.discard_max_bytes {
        bail!(
            "discard length {} exceeds discard max {}",
            length,
            capability.discard_max_bytes
        );
    }
    for info in list_devices()? {
        if info.path == device {
            if info.mounted {
                bail!("refusing discard on mounted device {}", device.display());
            }
            if info.is_root_device {
                bail!("refusing discard on root device {}", device.display());
            }
        }
        if info.parent.as_deref() == device.file_name().and_then(|v| v.to_str()) && info.mounted {
            bail!(
                "refusing discard on parent device {} because mounted partitions exist",
                device.display()
            );
        }
    }
    Ok(())
}

const BLKDISCARD: libc::c_ulong = 0x1277;
