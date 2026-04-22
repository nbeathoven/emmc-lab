use crate::system::{
    collect_capabilities, collect_system_snapshot, command_exists, detect_cqhci,
    read_mmc_host_state, AppPaths, MmcHostState, SystemSnapshot,
};
use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;

#[cfg(target_os = "linux")]
use crate::system::read_trimmed;
#[cfg(target_os = "linux")]
use std::fs::{self, File};
#[cfg(target_os = "linux")]
use std::io;
#[cfg(target_os = "linux")]
use std::mem::size_of;
#[cfg(target_os = "linux")]
use std::os::fd::AsRawFd;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct EmmcHealthSnapshot {
    pub collected_at: DateTime<Utc>,
    pub device: PathBuf,
    pub available: bool,
    pub device_life_time_est_typ_a: Option<String>,
    pub device_life_time_est_typ_b: Option<String>,
    pub pre_eol_info: Option<String>,
    pub raw_text: Option<String>,
    pub note: String,
    pub ext_csd_rev: Option<u8>,
    pub csd_structure: Option<u8>,
    pub sec_count: Option<u64>,
    pub bkops_support: Option<bool>,
    pub bkops_status: Option<u8>,
    pub bkops_urgency: Option<String>,
    pub cache_size_kb: Option<u64>,
    pub cache_ctrl: Option<u8>,
    pub cmdq_support: Option<u8>,
    pub cmdq_depth: Option<u8>,
    pub erase_group_size_bytes: Option<u64>,
    pub hc_erase_grp_size: Option<u64>,
    pub firmware_version: Option<String>,
    pub device_type: Option<u8>,
    pub page_size_kb: Option<u64>,
    pub pages_per_block: Option<u64>,
    pub vendor_info: Option<VendorSpecificInfo>,
    pub cid: Option<CidInfo>,
    pub ext_csd_source: Option<String>,
    pub ext_csd_raw: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthReport {
    pub system: SystemSnapshot,
    pub capabilities: crate::system::CapabilityReport,
    pub emmc: EmmcHealthSnapshot,
    #[serde(default)]
    pub mmc_host_state: Option<MmcHostState>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadDisturbInput {
    pub total_bytes_read: u64,
    pub page_size_kb: u64,
    pub pages_per_block: u64,
    pub read_disturb_threshold: u64,
    pub device_capacity_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadDisturbModel {
    pub estimated_reads_per_block: f64,
    pub threshold_ratio: f64,
    pub risk_label: String,
    pub caveats: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CidInfo {
    pub mid: u8,
    pub oid: String,
    pub pnm: String,
    pub prv: String,
    pub psn: String,
    pub mdt: String,
    pub raw_hex: String,
}

impl CidInfo {
    pub fn manufacturer_id_hex(&self) -> String {
        format!("0x{:02X}", self.mid)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "vendor")]
pub enum VendorSpecificInfo {
    Samsung { health_data: Vec<(String, String)> },
    SanDisk { health_data: Vec<(String, String)> },
    Micron { health_data: Vec<(String, String)> },
    Hynix { health_data: Vec<(String, String)> },
    Unknown { raw_bytes: Vec<u8> },
}

pub fn collect_health(
    paths: &AppPaths,
    device: &Path,
    page_size_kb: Option<u64>,
    pages_per_block: Option<u64>,
) -> Result<HealthReport> {
    let mut capabilities = collect_capabilities(paths);
    capabilities.cqhci = Some(detect_cqhci(device));
    Ok(HealthReport {
        system: collect_system_snapshot(),
        capabilities,
        emmc: read_emmc_health(device, page_size_kb, pages_per_block)?,
        mmc_host_state: read_mmc_host_state(device).ok(),
    })
}

pub fn read_emmc_health(
    device: &Path,
    page_size_kb: Option<u64>,
    pages_per_block: Option<u64>,
) -> Result<EmmcHealthSnapshot> {
    let now = Utc::now();
    let canonical_device = canonical_mmc_device_path(device);
    if !is_mmc_device(&canonical_device) {
        return Ok(unavailable_health_snapshot(
            now,
            device,
            "health telemetry is only available for /dev/mmcblk* devices",
        ));
    }

    // Prefer the raw register path first so we get stable structured data and avoid
    // depending on a userspace formatter. Keep mmc-utils as the last fallback for
    // systems where ioctl/debugfs access is blocked or not compiled in.
    match read_ext_csd_with_fallback(&canonical_device) {
        Ok((raw, source)) => {
            let mut snapshot = parse_ext_csd(&raw, page_size_kb, pages_per_block);
            snapshot.collected_at = now;
            snapshot.device = canonical_device.clone();
            snapshot.available = true;
            snapshot.ext_csd_source = Some(source.clone());
            snapshot.ext_csd_raw = Some(raw.to_vec());
            snapshot.raw_text = Some(format_raw_extcsd_dump(&raw));
            snapshot.csd_structure = read_csd_structure_sysfs(&canonical_device).ok().flatten();
            if let Ok(cid) = read_cid_sysfs(&canonical_device) {
                snapshot.vendor_info = Some(parse_vendor_specific(&raw, &cid));
                snapshot.cid = Some(cid);
            }
            snapshot.note = if cache_confirmed_absent(&raw) {
                format!(
                    "health values read from EXT_CSD via {source}; cache reported disabled/absent"
                )
            } else {
                format!("health values read from EXT_CSD via {source}")
            };
            Ok(snapshot)
        }
        Err(raw_err) => {
            let raw_err_text = format!("{raw_err:#}");
            if !command_exists("mmc") {
                return Ok(unavailable_health_snapshot(
                    now,
                    &canonical_device,
                    &format!(
                        "health telemetry unavailable: raw EXT_CSD read failed ({raw_err_text}); mmc-utils not found"
                    ),
                ));
            }

            // mmc-utils output is less complete than the raw path above, but it still gives
            // older systems a useful health snapshot instead of a hard failure.
            let output = Command::new("mmc")
                .arg("extcsd")
                .arg("read")
                .arg(&canonical_device)
                .output()
                .context("failed to execute mmc-utils")?;

            let raw_stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let raw_stderr = String::from_utf8_lossy(&output.stderr).to_string();
            if !output.status.success() {
                return Ok(EmmcHealthSnapshot {
                    collected_at: now,
                    device: canonical_device,
                    available: false,
                    raw_text: Some(format!("{}\n{}", raw_stdout, raw_stderr).trim().to_string()),
                    page_size_kb,
                    pages_per_block,
                    note: format!(
                        "health telemetry unavailable: raw EXT_CSD read failed ({raw_err_text}); mmc-utils fallback failed"
                    ),
                    ..EmmcHealthSnapshot::default()
                });
            }

            let combined = raw_stdout.trim().to_string();
            let mut snapshot = EmmcHealthSnapshot {
                collected_at: now,
                device: canonical_device.clone(),
                available: true,
                device_life_time_est_typ_a: parse_extcsd_field(
                    &combined,
                    "DEVICE_LIFE_TIME_EST_TYP_A",
                ),
                device_life_time_est_typ_b: parse_extcsd_field(
                    &combined,
                    "DEVICE_LIFE_TIME_EST_TYP_B",
                ),
                pre_eol_info: parse_extcsd_field(&combined, "PRE_EOL_INFO"),
                raw_text: Some(combined),
                page_size_kb,
                pages_per_block,
                ext_csd_source: Some("mmc-utils".to_string()),
                note: format!(
                    "health values are best-effort values reported by mmc-utils after raw EXT_CSD read failed ({raw_err_text})"
                ),
                ..EmmcHealthSnapshot::default()
            };
            snapshot.csd_structure = read_csd_structure_sysfs(&canonical_device).ok().flatten();
            if let Ok(cid) = read_cid_sysfs(&canonical_device) {
                snapshot.cid = Some(cid);
            }
            Ok(snapshot)
        }
    }
}

fn parse_extcsd_field(text: &str, key: &str) -> Option<String> {
    for line in text.lines() {
        if line.contains(key) {
            let value = line
                .split(':')
                .nth(1)
                .map(|s| s.trim().trim_matches(']').trim_matches('['))
                .filter(|v| !v.is_empty())
                .map(str::to_string);
            if value.is_some() {
                return value;
            }
        }
    }
    None
}

fn unavailable_health_snapshot(
    collected_at: DateTime<Utc>,
    device: &Path,
    note: &str,
) -> EmmcHealthSnapshot {
    EmmcHealthSnapshot {
        collected_at,
        device: device.to_path_buf(),
        available: false,
        note: note.to_string(),
        ..EmmcHealthSnapshot::default()
    }
}

fn is_mmc_device(device: &Path) -> bool {
    device
        .file_name()
        .map(|name| name.to_string_lossy().starts_with("mmcblk"))
        .unwrap_or(false)
}

fn canonical_mmc_device_path(device: &Path) -> PathBuf {
    let Some(name) = device
        .file_name()
        .map(|value| value.to_string_lossy().to_string())
    else {
        return device.to_path_buf();
    };
    if !name.starts_with("mmcblk") {
        return device.to_path_buf();
    }
    let Some(index) = name.rfind('p') else {
        return device.to_path_buf();
    };
    if name[index + 1..].chars().all(|ch| ch.is_ascii_digit()) {
        return device.with_file_name(&name[..index]);
    }
    device.to_path_buf()
}

fn parse_ext_csd(
    raw: &[u8; 512],
    page_size_kb: Option<u64>,
    pages_per_block: Option<u64>,
) -> EmmcHealthSnapshot {
    let firmware_bytes = &raw[254..262];
    let firmware_version = parse_ascii_trimmed(raw, 254, 8);
    let firmware_version = if firmware_version.is_empty() {
        Some(
            firmware_bytes
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<Vec<_>>()
                .join(""),
        )
    } else {
        Some(firmware_version)
    };
    let sec_count = parse_le_u32(raw, 212);
    let cache_size_kb = parse_le_u32(raw, 249) as u64;
    let bkops_status = raw[246];

    EmmcHealthSnapshot {
        available: true,
        device_life_time_est_typ_a: Some(format!("0x{:02x}", raw[268])),
        device_life_time_est_typ_b: Some(format!("0x{:02x}", raw[269])),
        pre_eol_info: Some(format!("0x{:02x}", raw[267])),
        ext_csd_rev: Some(raw[192]),
        sec_count: (sec_count != 0).then_some(sec_count as u64),
        bkops_support: Some(raw[502] != 0),
        bkops_status: Some(bkops_status),
        bkops_urgency: Some(bkops_urgency_label(bkops_status).to_string()),
        cache_size_kb: Some(cache_size_kb),
        cache_ctrl: Some(raw[33]),
        cmdq_support: Some(raw[308]),
        cmdq_depth: Some(raw[307]),
        erase_group_size_bytes: compute_erase_block_size_bytes(raw),
        hc_erase_grp_size: (raw[224] != 0).then_some(raw[224] as u64),
        firmware_version,
        device_type: Some(raw[196]),
        page_size_kb,
        pages_per_block,
        ..EmmcHealthSnapshot::default()
    }
}

#[allow(dead_code)]
fn parse_le_u16(raw: &[u8; 512], offset: usize) -> u16 {
    u16::from_le_bytes([raw[offset], raw[offset + 1]])
}

fn parse_le_u32(raw: &[u8; 512], offset: usize) -> u32 {
    u32::from_le_bytes([
        raw[offset],
        raw[offset + 1],
        raw[offset + 2],
        raw[offset + 3],
    ])
}

fn parse_ascii_trimmed(raw: &[u8; 512], start: usize, len: usize) -> String {
    raw[start..start + len]
        .iter()
        .copied()
        .filter(|byte| byte.is_ascii_graphic() || *byte == b' ')
        .map(char::from)
        .collect::<String>()
        .trim()
        .to_string()
}

fn bkops_urgency_label(status: u8) -> &'static str {
    match status {
        0 => "no operations needed",
        1 => "non-critical",
        2 => "performance impacted",
        3 => "critical",
        _ => "reserved",
    }
}

/// Build a host-side read-disturb estimate using operator-supplied geometry.
pub fn build_read_disturb_model(input: ReadDisturbInput) -> ReadDisturbModel {
    let mut caveats = vec![
        "This is a uniform-distribution host-side estimate. Actual NAND read counts depend on FTL behavior and are not observable from userspace.".to_string(),
    ];
    let block_size_bytes = input
        .page_size_kb
        .saturating_mul(1024)
        .saturating_mul(input.pages_per_block);
    if block_size_bytes == 0
        || input.device_capacity_bytes == 0
        || input.read_disturb_threshold == 0
    {
        caveats.push(
            "One or more required geometry inputs was zero, so the estimate could not be normalized."
                .to_string(),
        );
        return ReadDisturbModel {
            estimated_reads_per_block: 0.0,
            threshold_ratio: 0.0,
            risk_label: "unknown".to_string(),
            caveats,
        };
    }

    let estimated_reads_per_block =
        input.total_bytes_read as f64 / input.device_capacity_bytes as f64;
    let threshold_ratio = estimated_reads_per_block / input.read_disturb_threshold as f64;
    let risk_label = if threshold_ratio < 0.25 {
        "low"
    } else if threshold_ratio < 0.50 {
        "moderate"
    } else if threshold_ratio < 0.75 {
        "elevated"
    } else {
        "high"
    };
    caveats.push(
        "Kernel and page-cache effects can hide some device reads, so this estimate is conservative and may under-count NAND-side activity.".to_string(),
    );

    ReadDisturbModel {
        estimated_reads_per_block,
        threshold_ratio,
        risk_label: risk_label.to_string(),
        caveats,
    }
}

/// Compute the high-capacity erase group size in bytes from EXT_CSD.
pub fn compute_erase_block_size_bytes(raw: &[u8; 512]) -> Option<u64> {
    let groups = raw[224] as u64;
    (groups != 0).then_some(groups.saturating_mul(512 * 1024))
}

fn cache_confirmed_absent(raw: &[u8; 512]) -> bool {
    parse_le_u32(raw, 249) == 0 && raw[33] == 0
}

#[cfg(target_os = "linux")]
fn read_cid_sysfs(device: &Path) -> Result<CidInfo> {
    let sys_dir = block_device_sys_dir(device);
    let raw_hex = read_trimmed(sys_dir.join("device/cid"))
        .or_else(|| read_cid_from_bus_sysfs(device))
        .ok_or_else(|| anyhow!("missing CID sysfs entry for {}", device.display()))?;
    parse_cid_hex(&raw_hex)
}

#[cfg(not(target_os = "linux"))]
fn read_cid_sysfs(_device: &Path) -> Result<CidInfo> {
    Err(anyhow!("not supported on this platform"))
}

#[cfg(target_os = "linux")]
fn read_cid_from_bus_sysfs(device: &Path) -> Option<String> {
    let sys_dir = fs::canonicalize(block_device_sys_dir(device).join("device")).ok()?;
    let card = sys_dir.file_name()?.to_string_lossy().to_string();
    read_trimmed(Path::new("/sys/bus/mmc/devices").join(card).join("cid"))
}

/// Decode the 16-byte eMMC CID register from its 32-character sysfs hex form.
pub fn parse_cid_hex(raw_hex: &str) -> Result<CidInfo> {
    let cleaned = raw_hex
        .trim()
        .chars()
        .filter(|ch| !ch.is_ascii_whitespace() && *ch != ':')
        .collect::<String>();
    if cleaned.len() != 32 || !cleaned.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return Err(anyhow!("CID must be a 32-character hexadecimal string"));
    }
    let mut raw = [0_u8; 16];
    for (idx, byte) in raw.iter_mut().enumerate() {
        let start = idx * 2;
        *byte = u8::from_str_radix(&cleaned[start..start + 2], 16)
            .with_context(|| format!("invalid CID byte at offset {idx}"))?;
    }

    let pnm = raw[3..9]
        .iter()
        .copied()
        .filter(|byte| byte.is_ascii_graphic() || *byte == b' ')
        .map(char::from)
        .collect::<String>()
        .trim()
        .to_string();
    let psn = u32::from_be_bytes([raw[10], raw[11], raw[12], raw[13]]);
    let month = raw[14] & 0x0f;
    let year = 2013 + ((raw[14] >> 4) as u16);

    Ok(CidInfo {
        mid: raw[0],
        oid: raw[1..3]
            .iter()
            .map(|byte| format!("{byte:02X}"))
            .collect::<Vec<_>>()
            .join(""),
        pnm,
        prv: format!("{}.{}", raw[9] >> 4, raw[9] & 0x0f),
        psn: format!("0x{psn:08X}"),
        mdt: format_manufacturing_date(month, year),
        raw_hex: cleaned.to_ascii_lowercase(),
    })
}

fn format_manufacturing_date(month: u8, year: u16) -> String {
    let month_name = match month {
        1 => "January",
        2 => "February",
        3 => "March",
        4 => "April",
        5 => "May",
        6 => "June",
        7 => "July",
        8 => "August",
        9 => "September",
        10 => "October",
        11 => "November",
        12 => "December",
        _ => "Unknown",
    };
    if month_name == "Unknown" {
        format!("Unknown {year}")
    } else {
        format!("{month_name} {year}")
    }
}

fn parse_vendor_specific(raw: &[u8; 512], cid: &CidInfo) -> VendorSpecificInfo {
    let vendor_bytes = &raw[64..128];
    let summarize = |chunks: &[(usize, usize)]| {
        chunks
            .iter()
            .map(|(start, end)| {
                (
                    format!("bytes_{start}_{end}"),
                    vendor_bytes[*start - 64..*end - 63]
                        .iter()
                        .map(|byte| format!("{byte:02x}"))
                        .collect::<Vec<_>>()
                        .join(" "),
                )
            })
            .collect::<Vec<_>>()
    };
    match cid.mid {
        0x15 => VendorSpecificInfo::Samsung {
            health_data: summarize(&[(64, 67), (68, 71), (72, 79)]),
        },
        0x45 => VendorSpecificInfo::SanDisk {
            health_data: summarize(&[(64, 71), (72, 79)]),
        },
        0x13 => VendorSpecificInfo::Micron {
            health_data: summarize(&[(64, 65), (66, 71), (72, 79)]),
        },
        0x90 => VendorSpecificInfo::Hynix {
            health_data: summarize(&[(64, 71), (72, 79)]),
        },
        _ => VendorSpecificInfo::Unknown {
            raw_bytes: vendor_bytes.to_vec(),
        },
    }
}

/// Render a 16-byte-per-row EXT_CSD hex dump with lightweight field labels.
pub fn format_raw_extcsd_dump(raw: &[u8; 512]) -> String {
    let mut out = String::new();
    for offset in (0..raw.len()).step_by(16) {
        let bytes = &raw[offset..offset + 16];
        let hex = bytes
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<Vec<_>>()
            .join(" ");
        let ascii = bytes
            .iter()
            .map(|byte| {
                if byte.is_ascii_graphic() || *byte == b' ' {
                    *byte as char
                } else {
                    '.'
                }
            })
            .collect::<String>();
        let label = ext_csd_label(offset).unwrap_or("");
        out.push_str(&format!("{offset:03}: {hex:<47}  {ascii}"));
        if !label.is_empty() {
            out.push_str(&format!("  [{label}]"));
        }
        out.push('\n');
    }
    out
}

fn ext_csd_label(offset: usize) -> Option<&'static str> {
    match offset {
        32 => Some("CACHE_CTRL"),
        48 => Some("EXCEPTION_EVENTS"),
        160 => Some("BKOPS / BKOPS_AUTO_EN"),
        176 => Some("ERASE / POWER_OFF_NOTIFICATION"),
        192 => Some("EXT_CSD_REV / DEVICE_TYPE"),
        208 => Some("SEC_COUNT"),
        224 => Some("HC_ERASE_GRP_SIZE"),
        240 => Some("BKOPS_STATUS / CACHE_SIZE"),
        256 => Some("FW_VERSION"),
        _ => None,
    }
}

#[cfg(target_os = "linux")]
fn read_ext_csd_with_fallback(device: &Path) -> Result<([u8; 512], String)> {
    if let Ok(raw) = read_ext_csd_ioctl(device) {
        return Ok((raw, "ioctl".to_string()));
    }
    if let Ok(raw) = read_ext_csd_debugfs(device) {
        return Ok((raw, "sysfs".to_string()));
    }
    Err(anyhow!(
        "EXT_CSD raw read failed via ioctl and debugfs fallback"
    ))
}

#[cfg(not(target_os = "linux"))]
fn read_ext_csd_with_fallback(_device: &Path) -> Result<([u8; 512], String)> {
    Err(anyhow!("not supported on this platform"))
}

#[cfg(target_os = "linux")]
fn read_ext_csd_ioctl(device: &Path) -> Result<[u8; 512]> {
    let file = File::open(device)
        .with_context(|| format!("failed to open {} for EXT_CSD ioctl", device.display()))?;
    let mut raw = [0_u8; 512];
    let mut cmd = MmcIocCmd {
        write_flag: 0,
        is_acmd: 0,
        opcode: MMC_SEND_EXT_CSD,
        arg: 0,
        response: [0; 4],
        flags: MMC_RSP_SPI_R1 | MMC_RSP_R1 | MMC_CMD_ADTC,
        blksz: 512,
        blocks: 1,
        postsleep_min_us: 0,
        postsleep_max_us: 0,
        data_timeout_ns: 0,
        cmd_timeout_ms: 0,
        __pad: 0,
        data_ptr: raw.as_mut_ptr() as usize as u64,
    };
    let rc = unsafe { libc::ioctl(file.as_raw_fd(), MMC_IOC_CMD, &mut cmd) };
    if rc != 0 {
        return Err(io::Error::last_os_error())
            .with_context(|| format!("MMC_IOC_CMD failed for {}", device.display()));
    }
    Ok(raw)
}

#[cfg(target_os = "linux")]
fn read_ext_csd_debugfs(device: &Path) -> Result<[u8; 512]> {
    let sys_dir = fs::canonicalize(block_device_sys_dir(device).join("device"))
        .with_context(|| format!("failed to resolve sysfs device for {}", device.display()))?;
    let host = sys_dir
        .parent()
        .and_then(|path| path.file_name())
        .map(|value| value.to_string_lossy().to_string())
        .ok_or_else(|| anyhow!("failed to derive MMC host from sysfs path"))?;
    let card = sys_dir
        .file_name()
        .map(|value| value.to_string_lossy().to_string())
        .ok_or_else(|| anyhow!("failed to derive MMC card from sysfs path"))?;
    let candidates = [
        PathBuf::from("/sys/kernel/debug")
            .join(&host)
            .join(&card)
            .join("ext_csd"),
        PathBuf::from("/sys/kernel/debug")
            .join("mmc")
            .join(&host)
            .join(&card)
            .join("ext_csd"),
    ];
    for candidate in candidates {
        let Ok(text) = fs::read_to_string(&candidate) else {
            continue;
        };
        if let Some(raw) = parse_hex_bytes_512(&text) {
            return Ok(raw);
        }
    }
    Err(anyhow!("debugfs EXT_CSD not available"))
}

#[cfg(target_os = "linux")]
fn parse_hex_bytes_512(text: &str) -> Option<[u8; 512]> {
    let mut bytes = Vec::new();
    let mut pair = String::new();
    for ch in text.chars() {
        if ch.is_ascii_hexdigit() {
            pair.push(ch);
            if pair.len() == 2 {
                let byte = u8::from_str_radix(&pair, 16).ok()?;
                bytes.push(byte);
                pair.clear();
            }
        } else {
            pair.clear();
        }
    }
    if bytes.len() != 512 {
        return None;
    }
    let mut raw = [0_u8; 512];
    raw.copy_from_slice(&bytes);
    Some(raw)
}

#[cfg(target_os = "linux")]
fn read_csd_structure_sysfs(device: &Path) -> Result<Option<u8>> {
    let sys_dir = block_device_sys_dir(device);
    let Some(csd) = read_trimmed(sys_dir.join("device/csd")) else {
        return Ok(None);
    };
    let Some(first) = csd.chars().next() else {
        return Ok(None);
    };
    let nibble = first
        .to_digit(16)
        .ok_or_else(|| anyhow!("invalid CSD hex"))?;
    Ok(Some((nibble >> 2) as u8))
}

#[cfg(not(target_os = "linux"))]
fn read_csd_structure_sysfs(_device: &Path) -> Result<Option<u8>> {
    Err(anyhow!("not supported on this platform"))
}

#[cfg(target_os = "linux")]
fn block_device_sys_dir(device: &Path) -> PathBuf {
    let name = device
        .file_name()
        .map(|value| value.to_string_lossy().to_string())
        .unwrap_or_default();
    PathBuf::from("/sys/class/block").join(name)
}

#[cfg(target_os = "linux")]
const MMC_BLOCK_MAJOR: u32 = 179;
#[cfg(target_os = "linux")]
const IOC_WRITE: u32 = 1;
#[cfg(target_os = "linux")]
const IOC_READ: u32 = 2;
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
const MMC_CMD_ADTC: u32 = 1 << 5;
#[cfg(target_os = "linux")]
const MMC_RSP_PRESENT: u32 = 1 << 0;
#[cfg(target_os = "linux")]
const MMC_RSP_CRC: u32 = 1 << 2;
#[cfg(target_os = "linux")]
const MMC_RSP_OPCODE: u32 = 1 << 4;
#[cfg(target_os = "linux")]
const MMC_RSP_SPI_S1: u32 = 1 << 7;
#[cfg(target_os = "linux")]
const MMC_RSP_R1: u32 = MMC_RSP_PRESENT | MMC_RSP_CRC | MMC_RSP_OPCODE;
#[cfg(target_os = "linux")]
const MMC_RSP_SPI_R1: u32 = MMC_RSP_SPI_S1;
#[cfg(target_os = "linux")]
const MMC_SEND_EXT_CSD: u32 = 8;

#[cfg(target_os = "linux")]
const fn ioc(dir: u32, ty: u32, nr: u32, size: u32) -> libc::c_ulong {
    ((dir << IOC_DIRSHIFT) | (ty << IOC_TYPESHIFT) | (nr << IOC_NRSHIFT) | (size << IOC_SIZESHIFT))
        as libc::c_ulong
}

#[cfg(target_os = "linux")]
const fn iowr(ty: u32, nr: u32, size: u32) -> libc::c_ulong {
    ioc(IOC_READ | IOC_WRITE, ty, nr, size)
}

#[cfg(target_os = "linux")]
#[repr(C)]
struct MmcIocCmd {
    write_flag: i32,
    is_acmd: i32,
    opcode: u32,
    arg: u32,
    response: [u32; 4],
    flags: u32,
    blksz: u32,
    blocks: u32,
    postsleep_min_us: u32,
    postsleep_max_us: u32,
    data_timeout_ns: u32,
    cmd_timeout_ms: u32,
    __pad: u32,
    data_ptr: u64,
}

#[cfg(target_os = "linux")]
const MMC_IOC_CMD: libc::c_ulong = iowr(MMC_BLOCK_MAJOR, 0, size_of::<MmcIocCmd>() as u32);

#[cfg(test)]
mod tests {
    use super::{
        bkops_urgency_label, build_read_disturb_model, compute_erase_block_size_bytes,
        parse_cid_hex, parse_ext_csd, parse_extcsd_field, parse_vendor_specific, CidInfo,
        ReadDisturbInput, VendorSpecificInfo,
    };

    #[test]
    fn parses_extcsd_health_fields() {
        let sample = r#"
[PRE_EOL_INFO: 0x01]
[DEVICE_LIFE_TIME_EST_TYP_A: 0x05]
[DEVICE_LIFE_TIME_EST_TYP_B: 0x06]
"#;
        assert_eq!(
            parse_extcsd_field(sample, "DEVICE_LIFE_TIME_EST_TYP_A").as_deref(),
            Some("0x05")
        );
    }

    #[test]
    fn parses_structured_ext_csd_fields() {
        let mut raw = [0_u8; 512];
        raw[192] = 8;
        raw[196] = 0x57;
        raw[212..216].copy_from_slice(&0x0010_0000_u32.to_le_bytes());
        raw[224] = 8;
        raw[246] = 2;
        raw[249..253].copy_from_slice(&0_u32.to_le_bytes());
        raw[254..262].copy_from_slice(b"FW123456");
        raw[307] = 31;
        raw[308] = 1;
        raw[267] = 1;
        raw[268] = 3;
        raw[269] = 4;
        raw[502] = 1;
        let snapshot = parse_ext_csd(&raw, Some(16), Some(256));
        assert_eq!(snapshot.ext_csd_rev, Some(8));
        assert_eq!(snapshot.device_type, Some(0x57));
        assert_eq!(snapshot.sec_count, Some(0x0010_0000));
        assert_eq!(snapshot.bkops_status, Some(2));
        assert_eq!(
            snapshot.bkops_urgency.as_deref(),
            Some("performance impacted")
        );
        assert_eq!(snapshot.device_life_time_est_typ_a.as_deref(), Some("0x03"));
        assert_eq!(snapshot.device_life_time_est_typ_b.as_deref(), Some("0x04"));
        assert_eq!(snapshot.pre_eol_info.as_deref(), Some("0x01"));
        assert_eq!(snapshot.hc_erase_grp_size, Some(8));
        assert_eq!(snapshot.erase_group_size_bytes, Some(4_194_304));
        assert_eq!(snapshot.firmware_version.as_deref(), Some("FW123456"));
        assert_eq!(snapshot.page_size_kb, Some(16));
        assert_eq!(snapshot.pages_per_block, Some(256));
        assert_eq!(snapshot.cache_size_kb, Some(0));
        assert_eq!(snapshot.cmdq_support, Some(1));
        assert_eq!(snapshot.cmdq_depth, Some(31));
        assert_eq!(snapshot.bkops_support, Some(true));
        assert_eq!(compute_erase_block_size_bytes(&raw), Some(4_194_304));
    }

    #[test]
    fn maps_bkops_urgency_labels() {
        assert_eq!(bkops_urgency_label(0), "no operations needed");
        assert_eq!(bkops_urgency_label(1), "non-critical");
        assert_eq!(bkops_urgency_label(2), "performance impacted");
        assert_eq!(bkops_urgency_label(3), "critical");
    }

    #[test]
    fn parses_vendor_specific_blocks() {
        let mut raw = [0_u8; 512];
        for (index, byte) in raw[64..128].iter_mut().enumerate() {
            *byte = index as u8;
        }
        let samsung_cid = CidInfo {
            mid: 0x15,
            oid: "0x0000".to_string(),
            pnm: "TEST".to_string(),
            prv: "0x0".to_string(),
            psn: "0x0".to_string(),
            mdt: "01/2026".to_string(),
            raw_hex: "00".to_string(),
        };
        match parse_vendor_specific(&raw, &samsung_cid) {
            VendorSpecificInfo::Samsung { health_data } => assert!(!health_data.is_empty()),
            other => panic!("unexpected vendor decode: {other:?}"),
        }

        let unknown_cid = CidInfo {
            mid: 0xff,
            ..samsung_cid
        };
        match parse_vendor_specific(&raw, &unknown_cid) {
            VendorSpecificInfo::Unknown { raw_bytes } => assert_eq!(raw_bytes.len(), 64),
            other => panic!("unexpected vendor decode: {other:?}"),
        }
    }

    #[test]
    fn parses_runtime_cid_register() {
        let cid = parse_cid_hex("0101005334303030340148190d528700").expect("cid");
        assert_eq!(cid.mid, 0x01);
        assert_eq!(cid.manufacturer_id_hex(), "0x01");
        assert_eq!(cid.pnm, "S40004");
        assert_eq!(cid.psn, "0x48190D52");
        assert_eq!(cid.mdt, "July 2021");
        assert_eq!(cid.raw_hex, "0101005334303030340148190d528700");
    }

    #[test]
    fn rejects_malformed_cid_registers() {
        assert!(parse_cid_hex("0101").is_err());
        assert!(parse_cid_hex("zz01005334303030340148190d528700").is_err());
    }

    #[test]
    fn read_disturb_model_scales_and_labels_risk() {
        let model = build_read_disturb_model(ReadDisturbInput {
            total_bytes_read: 2_000_000,
            page_size_kb: 16,
            pages_per_block: 256,
            read_disturb_threshold: 2,
            device_capacity_bytes: 4_000_000,
        });
        assert_eq!(model.estimated_reads_per_block, 0.5);
        assert_eq!(model.threshold_ratio, 0.25);
        assert_eq!(model.risk_label, "moderate");
        assert!(!model.caveats.is_empty());
    }

    #[test]
    fn read_disturb_model_handles_zero_inputs() {
        let model = build_read_disturb_model(ReadDisturbInput {
            total_bytes_read: 0,
            page_size_kb: 0,
            pages_per_block: 256,
            read_disturb_threshold: 100_000,
            device_capacity_bytes: 0,
        });
        assert_eq!(model.risk_label, "unknown");
        assert_eq!(model.threshold_ratio, 0.0);
        assert!(!model.caveats.is_empty());
    }
}
