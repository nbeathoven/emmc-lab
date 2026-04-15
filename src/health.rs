use crate::discard::{detect_discard, DiscardCapability};
use crate::system::{
    collect_capabilities, collect_system_snapshot, command_exists, AppPaths, SystemSnapshot,
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
    pub erase_group_size_bytes: Option<u64>,
    pub hc_erase_grp_size: Option<u64>,
    pub firmware_version: Option<String>,
    pub device_type: Option<u8>,
    pub page_size_kb: Option<u64>,
    pub pages_per_block: Option<u64>,
    pub vendor_info: Option<VendorSpecificInfo>,
    pub cid: Option<CidInfo>,
    pub csd: Option<CsdInfo>,
    pub ext_csd_decoded: Option<ExtCsdDecoded>,
    pub partition_info: Option<MmcPartitionInfo>,
    pub health_summary: Vec<(String, String)>,
    pub ext_csd_source: Option<String>,
    pub ext_csd_raw: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthReport {
    pub system: SystemSnapshot,
    pub capabilities: crate::system::CapabilityReport,
    pub emmc: EmmcHealthSnapshot,
    pub discard: Option<DiscardCapability>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadDisturbInput {
    pub total_bytes_read: u64,
    pub total_bytes_written: u64,
    pub total_bytes_discarded: u64,
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
pub enum ReadDisturbRisk {
    Low,
    Moderate,
    Elevated,
    High,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnduranceModel {
    pub device_capacity_bytes: u64,
    pub erase_block_size_bytes: u64,
    pub page_size_bytes: u64,
    pub pages_per_block: u64,
    pub total_bytes_written: u64,
    pub total_bytes_read: u64,
    pub total_bytes_discarded: u64,
    pub total_blocks: u64,
    pub host_write_amplification_estimate: f64,
    pub estimated_program_erase_cycles: f64,
    pub reads_per_block_estimate: f64,
    pub read_disturb_risk: ReadDisturbRisk,
    pub read_disturb_reads_per_block: f64,
    pub read_disturb_threshold: u64,
    pub read_disturb_ratio: f64,
    pub caveats: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct CidInfo {
    pub mid: u8,
    pub oid: String,
    pub pnm: String,
    pub prv: String,
    pub psn: String,
    pub mdt: String,
    pub cbx: Option<u8>,
    pub prv_major: Option<u8>,
    pub prv_minor: Option<u8>,
    pub psn_numeric: Option<u32>,
    pub mdt_year: Option<u16>,
    pub mdt_month: Option<u8>,
    pub crc: Option<u8>,
    pub raw_hex: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct CsdInfo {
    pub csd_structure: u8,
    pub spec_vers: u8,
    pub taac: u8,
    pub nsac: u8,
    pub tran_speed: u8,
    pub ccc: u16,
    pub read_bl_len: u8,
    pub read_bl_partial: bool,
    pub write_blk_misalign: bool,
    pub read_blk_misalign: bool,
    pub dsr_imp: bool,
    pub c_size: u32,
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

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ExtCsdDecoded {
    pub ext_csd_rev: u8,
    pub device_life_time_est_typ_a: u8,
    pub device_life_time_est_typ_b: u8,
    pub pre_eol_info: u8,
    pub sec_count: u32,
    pub firmware_version: String,
    pub boot_size_mult: u8,
    pub boot_info: u8,
    pub partition_config: u8,
    pub boot_bus_conditions: u8,
    pub rst_n_function: u8,
    pub rpmb_size_mult: u8,
    pub partitioning_support: u8,
    pub max_enh_size_mult: u32,
    pub partitions_attribute: u8,
    pub partition_setting_completed: u8,
    pub gp_size_mult: [[u8; 3]; 4],
    pub cache_size: u32,
    pub cache_ctrl: u8,
    pub cache_flushed: bool,
    pub bkops_en: u8,
    pub bkops_start: u8,
    pub bkops_support: u8,
    pub bkops_status: u8,
    pub hpi_features: u8,
    pub hpi_mgmt: u8,
    pub erase_group_def: u8,
    pub hc_erase_grp_size: u8,
    pub hc_wp_grp_size: u8,
    pub rel_wr_sec_c: u8,
    pub wr_rel_set: u8,
    pub wr_rel_param: u8,
    pub device_type: u8,
    pub hs_timing: u8,
    pub bus_width: u8,
    pub strobe_support: u8,
    pub power_class: u8,
    pub power_off_notification: u8,
    pub power_off_long_time: u8,
    pub sleep_notification_time: u8,
    pub cmdq_support: u8,
    pub cmdq_depth: u8,
    pub secure_removal_type: u8,
    pub secure_feature_support: u8,
    pub sec_trim_mult: u8,
    pub sec_erase_mult: u8,
    pub ffu_features: u8,
    pub supported_modes: u8,
    pub ffu_arg: u32,
    pub number_of_fw_sectors_correctly_programmed: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct GpPartitionInfo {
    pub index: usize,
    pub size_mult: [u8; 3],
    pub raw_size_units: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct MmcPartitionInfo {
    pub boot_partition_1_size_kb: u64,
    pub boot_partition_2_size_kb: u64,
    pub rpmb_size_kb: u64,
    pub boot_partition_enabled: bool,
    pub boot_ack_enabled: bool,
    pub access_partition: u8,
    pub gp_partitions: Vec<GpPartitionInfo>,
    pub enhanced_area_size_bytes: Option<u64>,
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
    Ok(HealthReport {
        system: collect_system_snapshot(),
        capabilities: collect_capabilities(paths),
        emmc: read_emmc_health(device, page_size_kb, pages_per_block)?,
        discard: detect_discard(device).ok(),
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
            snapshot.csd = read_csd_sysfs(&canonical_device).ok();
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
            let raw_err_reason = classify_ext_csd_failure(&raw_err_text);
            if !command_exists("mmc") {
                return Ok(unavailable_health_snapshot(
                    now,
                    &canonical_device,
                    &format!("health telemetry unavailable: {raw_err_reason}; mmc-utils not found"),
                ));
            }
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
                        "health telemetry unavailable: {raw_err_reason}; mmc-utils fallback failed"
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
                    "health values are best-effort values reported by mmc-utils after {raw_err_reason}"
                ),
                ..EmmcHealthSnapshot::default()
            };
            snapshot.csd_structure = read_csd_structure_sysfs(&canonical_device).ok().flatten();
            snapshot.csd = read_csd_sysfs(&canonical_device).ok();
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

fn classify_ext_csd_failure(detail: &str) -> String {
    let lower = detail.to_ascii_lowercase();
    if lower.contains("permission denied") || lower.contains("operation not permitted") {
        "EXT_CSD access was denied by permissions".to_string()
    } else if lower.contains("timed out") || lower.contains("connection timed out") {
        "EXT_CSD command timed out at the MMC/kernel layer; this is not a sudo issue".to_string()
    } else if lower.contains("not supported") || lower.contains("inappropriate ioctl") {
        "EXT_CSD access is not supported by this kernel/device path".to_string()
    } else {
        format!("raw EXT_CSD read failed ({detail})")
    }
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
    let decoded = decode_ext_csd(raw);
    let sec_count = decoded.sec_count as u64;
    let cache_size_kb = decoded.cache_size as u64;
    let bkops_status = decoded.bkops_status;
    let partition_info = derive_mmc_partition_info(&decoded, raw);
    let health_summary = build_health_summary(&decoded);

    EmmcHealthSnapshot {
        available: true,
        device_life_time_est_typ_a: Some(format!("0x{:02x}", decoded.device_life_time_est_typ_a)),
        device_life_time_est_typ_b: Some(format!("0x{:02x}", decoded.device_life_time_est_typ_b)),
        pre_eol_info: Some(format!("0x{:02x}", decoded.pre_eol_info)),
        ext_csd_rev: Some(decoded.ext_csd_rev),
        sec_count: (sec_count != 0).then_some(sec_count as u64),
        bkops_support: Some(decoded.bkops_support != 0),
        bkops_status: Some(bkops_status),
        bkops_urgency: Some(bkops_urgency_label(bkops_status).to_string()),
        cache_size_kb: Some(cache_size_kb),
        cache_ctrl: Some(decoded.cache_ctrl),
        erase_group_size_bytes: compute_erase_block_size_bytes(raw),
        hc_erase_grp_size: (decoded.hc_erase_grp_size != 0)
            .then_some(decoded.hc_erase_grp_size as u64),
        firmware_version: Some(decoded.firmware_version.clone()),
        device_type: Some(decoded.device_type),
        page_size_kb,
        pages_per_block,
        ext_csd_decoded: Some(decoded),
        partition_info: Some(partition_info),
        health_summary,
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

fn parse_le_u24(raw: &[u8; 512], offset: usize) -> u32 {
    u32::from_le_bytes([raw[offset], raw[offset + 1], raw[offset + 2], 0])
}

pub fn decode_life_time_estimate(value: u8) -> String {
    match value {
        0x00 => "Not defined".to_string(),
        0x01..=0x0A => format!("{}%–{}% life used", (value - 1) * 10, value * 10),
        0x0B => "Exceeded maximum estimated life".to_string(),
        _ => format!("Reserved (0x{value:02X})"),
    }
}

pub fn decode_pre_eol_info(value: u8) -> &'static str {
    match value {
        0x00 => "Not defined",
        0x01 => "Normal",
        0x02 => "Warning (consumed 80% of reserved blocks)",
        0x03 => "Urgent (consumed 90% of reserved blocks)",
        _ => "Reserved",
    }
}

pub fn decode_ext_csd(raw: &[u8; 512]) -> ExtCsdDecoded {
    let firmware_version = parse_ascii_trimmed(raw, 254, 8);
    ExtCsdDecoded {
        ext_csd_rev: raw[192],
        device_life_time_est_typ_a: raw[268],
        device_life_time_est_typ_b: raw[269],
        pre_eol_info: raw[267],
        sec_count: parse_le_u32(raw, 212),
        firmware_version: if firmware_version.is_empty() {
            raw[254..262]
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<Vec<_>>()
                .join("")
        } else {
            firmware_version
        },
        boot_size_mult: raw[226],
        boot_info: raw[228],
        partition_config: raw[179],
        boot_bus_conditions: raw[177],
        rst_n_function: raw[162],
        rpmb_size_mult: raw[168],
        partitioning_support: raw[160],
        max_enh_size_mult: parse_le_u24(raw, 157),
        partitions_attribute: raw[156],
        partition_setting_completed: raw[155],
        gp_size_mult: [
            [raw[143], raw[144], raw[145]],
            [raw[146], raw[147], raw[148]],
            [raw[149], raw[150], raw[151]],
            [raw[152], raw[153], raw[154]],
        ],
        cache_size: parse_le_u32(raw, 249),
        cache_ctrl: raw[33],
        cache_flushed: raw[32] != 0,
        bkops_en: raw[163],
        bkops_start: raw[164],
        bkops_support: raw[502],
        bkops_status: raw[246],
        hpi_features: raw[503],
        hpi_mgmt: raw[161],
        erase_group_def: raw[175],
        hc_erase_grp_size: raw[224],
        hc_wp_grp_size: raw[221],
        rel_wr_sec_c: raw[222],
        wr_rel_set: raw[167],
        wr_rel_param: raw[166],
        device_type: raw[196],
        hs_timing: raw[185],
        bus_width: raw[183],
        strobe_support: raw[184],
        power_class: raw[187],
        power_off_notification: raw[34],
        power_off_long_time: raw[247],
        sleep_notification_time: raw[216],
        cmdq_support: raw[308],
        cmdq_depth: raw[307],
        secure_removal_type: raw[16],
        secure_feature_support: raw[231],
        sec_trim_mult: raw[229],
        sec_erase_mult: raw[230],
        ffu_features: raw[492],
        supported_modes: raw[493],
        ffu_arg: parse_le_u32(raw, 487),
        number_of_fw_sectors_correctly_programmed: parse_le_u32(raw, 302),
    }
}

fn derive_mmc_partition_info(decoded: &ExtCsdDecoded, raw: &[u8; 512]) -> MmcPartitionInfo {
    let gp_partitions = decoded
        .gp_size_mult
        .iter()
        .enumerate()
        .map(|(index, bytes)| GpPartitionInfo {
            index: index + 1,
            size_mult: *bytes,
            raw_size_units: u32::from_le_bytes([bytes[0], bytes[1], bytes[2], 0]),
        })
        .filter(|part| part.raw_size_units != 0)
        .collect::<Vec<_>>();
    let enhanced_units = decoded.max_enh_size_mult as u64;
    MmcPartitionInfo {
        boot_partition_1_size_kb: decoded.boot_size_mult as u64 * 128,
        boot_partition_2_size_kb: decoded.boot_size_mult as u64 * 128,
        rpmb_size_kb: decoded.rpmb_size_mult as u64 * 128,
        boot_partition_enabled: (decoded.partition_config & 0x38) != 0,
        boot_ack_enabled: (decoded.partition_config & 0x40) != 0,
        access_partition: decoded.partition_config & 0x7,
        gp_partitions,
        enhanced_area_size_bytes: if enhanced_units == 0 {
            None
        } else {
            Some(
                enhanced_units
                    .saturating_mul(raw[224].max(1) as u64)
                    .saturating_mul(512 * 1024),
            )
        },
    }
}

fn build_health_summary(decoded: &ExtCsdDecoded) -> Vec<(String, String)> {
    vec![
        (
            "Life A".to_string(),
            decode_life_time_estimate(decoded.device_life_time_est_typ_a),
        ),
        (
            "Life B".to_string(),
            decode_life_time_estimate(decoded.device_life_time_est_typ_b),
        ),
        (
            "Pre-EOL".to_string(),
            decode_pre_eol_info(decoded.pre_eol_info).to_string(),
        ),
        (
            "Boot partitions".to_string(),
            format!("{} KiB each", decoded.boot_size_mult as u64 * 128),
        ),
        (
            "RPMB".to_string(),
            format!("{} KiB", decoded.rpmb_size_mult as u64 * 128),
        ),
        (
            "Cache".to_string(),
            format!("{} KiB / ctrl {}", decoded.cache_size, decoded.cache_ctrl),
        ),
        (
            "Command queue".to_string(),
            if decoded.cmdq_support != 0 {
                format!("supported, depth {}", decoded.cmdq_depth)
            } else {
                "not supported".to_string()
            },
        ),
    ]
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

pub fn build_endurance_model(input: ReadDisturbInput) -> EnduranceModel {
    let page_size_bytes = input.page_size_kb.saturating_mul(1024);
    let erase_block_size_bytes = page_size_bytes.saturating_mul(input.pages_per_block);
    let total_blocks = if erase_block_size_bytes == 0 {
        0
    } else {
        input.device_capacity_bytes / erase_block_size_bytes.max(1)
    };
    let estimated_program_erase_cycles = if input.device_capacity_bytes == 0 {
        0.0
    } else {
        input.total_bytes_written as f64 / input.device_capacity_bytes as f64
    };
    let reads_per_block_estimate = if total_blocks == 0 {
        0.0
    } else {
        input.total_bytes_read as f64 / total_blocks as f64
    };
    let read_disturb_ratio = if input.read_disturb_threshold == 0 {
        0.0
    } else {
        reads_per_block_estimate / input.read_disturb_threshold as f64
    };
    let read_disturb_risk = if input.read_disturb_threshold == 0 || total_blocks == 0 {
        ReadDisturbRisk::Unknown
    } else if read_disturb_ratio < 0.3 {
        ReadDisturbRisk::Low
    } else if read_disturb_ratio < 0.7 {
        ReadDisturbRisk::Moderate
    } else if read_disturb_ratio < 1.0 {
        ReadDisturbRisk::Elevated
    } else {
        ReadDisturbRisk::High
    };
    EnduranceModel {
        device_capacity_bytes: input.device_capacity_bytes,
        erase_block_size_bytes,
        page_size_bytes,
        pages_per_block: input.pages_per_block,
        total_bytes_written: input.total_bytes_written,
        total_bytes_read: input.total_bytes_read,
        total_bytes_discarded: input.total_bytes_discarded,
        total_blocks,
        host_write_amplification_estimate: 1.0,
        estimated_program_erase_cycles,
        reads_per_block_estimate,
        read_disturb_risk,
        read_disturb_reads_per_block: reads_per_block_estimate,
        read_disturb_threshold: input.read_disturb_threshold,
        read_disturb_ratio,
        caveats: vec![
            "All estimates are based on host-visible logical I/O, not physical NAND operations"
                .to_string(),
            "FTL wear-leveling, garbage collection, and write amplification are not visible"
                .to_string(),
            "Actual NAND wear may be 2-10x higher than host-visible writes suggest".to_string(),
            "Read-disturb thresholds vary by NAND technology (SLC/MLC/TLC) and are not readable"
                .to_string(),
        ],
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
        .ok_or_else(|| anyhow!("missing CID sysfs entry for {}", device.display()))?;
    let raw_bytes = parse_hex_register(&raw_hex)?;
    let mid = read_trimmed(sys_dir.join("device/manfid"))
        .and_then(|value| parse_sysfs_hex_u64(&value))
        .map(|value| value as u8)
        .ok_or_else(|| anyhow!("missing or invalid manfid sysfs entry"))?;
    let prv = raw_bytes[6];
    let mdt_raw = ((raw_bytes[13] as u16) << 8) | raw_bytes[14] as u16;
    let mdt_month = (mdt_raw & 0x0F) as u8;
    let mdt_year = 1997 + ((mdt_raw >> 4) & 0xFF);
    Ok(CidInfo {
        mid,
        oid: read_trimmed(sys_dir.join("device/oemid")).unwrap_or_else(|| "-".to_string()),
        pnm: read_trimmed(sys_dir.join("device/name")).unwrap_or_else(|| "-".to_string()),
        prv: format!("{}.{}", prv >> 4, prv & 0x0F),
        psn: format!(
            "0x{:08x}",
            u32::from_be_bytes([raw_bytes[7], raw_bytes[8], raw_bytes[9], raw_bytes[10]])
        ),
        mdt: format!("{mdt_month:02}/{mdt_year}"),
        cbx: Some((raw_bytes[1] >> 6) & 0x03),
        prv_major: Some(prv >> 4),
        prv_minor: Some(prv & 0x0F),
        psn_numeric: Some(u32::from_be_bytes([
            raw_bytes[7],
            raw_bytes[8],
            raw_bytes[9],
            raw_bytes[10],
        ])),
        mdt_year: Some(mdt_year),
        mdt_month: Some(mdt_month),
        crc: Some(raw_bytes[15] >> 1),
        raw_hex,
    })
}

#[cfg(not(target_os = "linux"))]
fn read_cid_sysfs(_device: &Path) -> Result<CidInfo> {
    Err(anyhow!("not supported on this platform"))
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

#[cfg(target_os = "linux")]
fn read_csd_sysfs(device: &Path) -> Result<CsdInfo> {
    let sys_dir = block_device_sys_dir(device);
    let raw_hex = read_trimmed(sys_dir.join("device/csd"))
        .ok_or_else(|| anyhow!("missing CSD sysfs entry for {}", device.display()))?;
    let raw = parse_hex_register(&raw_hex)?;
    let bits = |msb: usize, lsb: usize| extract_bits_be(&raw, msb, lsb);
    Ok(CsdInfo {
        csd_structure: bits(127, 126) as u8,
        spec_vers: bits(125, 122) as u8,
        taac: bits(119, 112) as u8,
        nsac: bits(111, 104) as u8,
        tran_speed: bits(103, 96) as u8,
        ccc: bits(95, 84) as u16,
        read_bl_len: bits(83, 80) as u8,
        read_bl_partial: bits(79, 79) != 0,
        write_blk_misalign: bits(78, 78) != 0,
        read_blk_misalign: bits(77, 77) != 0,
        dsr_imp: bits(76, 76) != 0,
        c_size: bits(73, 62) as u32,
        vdd_r_curr_min: bits(61, 59) as u8,
        vdd_r_curr_max: bits(58, 56) as u8,
        vdd_w_curr_min: bits(55, 53) as u8,
        vdd_w_curr_max: bits(52, 50) as u8,
        c_size_mult: bits(49, 47) as u8,
        erase_grp_size: bits(46, 42) as u8,
        erase_grp_mult: bits(41, 37) as u8,
        wp_grp_size: bits(36, 32) as u8,
        wp_grp_enable: bits(31, 31) != 0,
        r2w_factor: bits(28, 26) as u8,
        write_bl_len: bits(25, 22) as u8,
        write_bl_partial: bits(21, 21) != 0,
        content_prot_app: bits(16, 16) != 0,
        file_format_grp: bits(15, 15) != 0,
        copy: bits(14, 14) != 0,
        perm_write_protect: bits(13, 13) != 0,
        tmp_write_protect: bits(12, 12) != 0,
        file_format: bits(11, 10) as u8,
        raw_hex,
    })
}

#[cfg(not(target_os = "linux"))]
fn read_csd_sysfs(_device: &Path) -> Result<CsdInfo> {
    Err(anyhow!("not supported on this platform"))
}

#[cfg(target_os = "linux")]
fn parse_hex_register(value: &str) -> Result<[u8; 16]> {
    let trimmed = value.trim();
    if trimmed.len() != 32 {
        return Err(anyhow!("expected 32 hex chars, got {}", trimmed.len()));
    }
    let mut raw = [0_u8; 16];
    for (index, chunk) in trimmed.as_bytes().chunks(2).enumerate() {
        let text = std::str::from_utf8(chunk).context("invalid register hex")?;
        raw[index] = u8::from_str_radix(text, 16).context("invalid register hex byte")?;
    }
    Ok(raw)
}

#[cfg(target_os = "linux")]
fn extract_bits_be(raw: &[u8; 16], msb: usize, lsb: usize) -> u64 {
    let mut value = 0_u64;
    for bit in (lsb..=msb).rev() {
        let byte_index = 15 - (bit / 8);
        let bit_index = bit % 8;
        let bit_value = (raw[byte_index] >> bit_index) & 1;
        value = (value << 1) | bit_value as u64;
    }
    value
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
fn parse_sysfs_hex_u64(value: &str) -> Option<u64> {
    value
        .trim()
        .trim_start_matches("0x")
        .trim_start_matches("0X")
        .parse::<u64>()
        .ok()
        .or_else(|| u64::from_str_radix(value.trim().trim_start_matches("0x"), 16).ok())
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
        parse_ext_csd, parse_extcsd_field, parse_vendor_specific, CidInfo, ReadDisturbInput,
        VendorSpecificInfo,
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
            ..CidInfo::default()
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
    fn read_disturb_model_scales_and_labels_risk() {
        let model = build_read_disturb_model(ReadDisturbInput {
            total_bytes_read: 2_000_000,
            total_bytes_written: 1_000_000,
            total_bytes_discarded: 0,
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
            total_bytes_written: 0,
            total_bytes_discarded: 0,
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
