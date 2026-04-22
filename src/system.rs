use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::env;
use std::fs;
use std::io;
#[cfg(target_os = "linux")]
use std::os::fd::AsRawFd;
use std::os::fd::RawFd;
use std::os::unix::fs::FileTypeExt;
use std::path::{Path, PathBuf};
#[cfg(target_os = "linux")]
use std::process;

use crate::boot_partition::list_boot_partitions;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppPaths {
    pub base_dir: PathBuf,
    pub profiles_dir: PathBuf,
    pub sessions_dir: PathBuf,
    pub exports_dir: PathBuf,
    pub samples_dir: PathBuf,
}

impl AppPaths {
    pub fn discover() -> Result<Self> {
        let home = env::var("HOME").unwrap_or_else(|_| ".".to_string());
        let config_home = env::var("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(&home).join(".config"));
        let data_home = env::var("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(&home).join(".local/share"));
        let base_dir = data_home.join("emmc-lab");
        let profiles_dir = config_home.join("emmc-lab/profiles");
        let sessions_dir = base_dir.join("sessions");
        let exports_dir = base_dir.join("exports");
        let samples_dir = base_dir.join("samples");
        fs::create_dir_all(&profiles_dir)?;
        fs::create_dir_all(&sessions_dir)?;
        fs::create_dir_all(&exports_dir)?;
        fs::create_dir_all(&samples_dir)?;
        Ok(Self {
            base_dir,
            profiles_dir,
            sessions_dir,
            exports_dir,
            samples_dir,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SystemSnapshot {
    pub collected_at: DateTime<Utc>,
    pub kernel_version: Option<String>,
    pub hostname: Option<String>,
    pub cpu_temperature_c: Option<f32>,
    pub cpu_frequency_mhz: Option<u64>,
    pub mem_total_kb: Option<u64>,
    pub mem_available_kb: Option<u64>,
    pub load_average: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DeviceInfo {
    pub path: PathBuf,
    pub name: String,
    #[serde(default)]
    pub parent: Option<String>,
    #[serde(default)]
    pub is_partition: bool,
    pub media_type: Option<String>,
    pub model: Option<String>,
    pub size_bytes: Option<u64>,
    pub removable: Option<bool>,
    pub mounted: bool,
    pub mountpoints: Vec<String>,
    pub filesystem_types: Vec<String>,
    pub mount_options: Vec<String>,
    pub is_root_device: bool,
    pub logical_block_size: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityReport {
    pub io_uring_available: bool,
    pub direct_io_available: bool,
    pub deep_trace_available: bool,
    pub mmc_health_available: bool,
    pub procfs_access: bool,
    pub permission_level: String,
    #[serde(default)]
    pub cqhci: Option<CqhciStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DoctorStatus {
    #[default]
    Pass,
    Warn,
    Fail,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoctorCheck {
    pub name: String,
    #[serde(default)]
    pub status: DoctorStatus,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoctorReport {
    pub generated_at: DateTime<Utc>,
    pub checks: Vec<DoctorCheck>,
    pub devices: Vec<DeviceInfo>,
    pub capabilities: CapabilityReport,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SafetyAssessment {
    pub allowed: bool,
    pub reasons: Vec<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CqhciStatus {
    pub mmc_host: String,
    pub cmdq_enabled: bool,
    pub cmdq_depth: Option<u8>,
    pub driver_name: Option<String>,
    pub kernel_version: String,
    pub risk: CqhciRisk,
    pub warning: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum CqhciRisk {
    None,
    Low,
    High,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MmcHostState {
    pub mmc_host: String,
    pub card_present: bool,
    pub card_type: Option<String>,
    pub card_state: Option<String>,
    pub ios_clock_hz: Option<u64>,
    pub ios_bus_width: Option<u8>,
    pub ios_timing: Option<String>,
    pub ocr: Option<u32>,
    pub cmdq_enabled: Option<bool>,
    pub read_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
struct MountInfo {
    source: String,
    mountpoint: String,
    fs_type: String,
    mount_options: String,
    major_minor: String,
}

pub fn collect_system_snapshot() -> SystemSnapshot {
    SystemSnapshot {
        collected_at: Utc::now(),
        kernel_version: read_trimmed("/proc/sys/kernel/osrelease"),
        hostname: read_trimmed("/proc/sys/kernel/hostname"),
        cpu_temperature_c: read_cpu_temp(),
        cpu_frequency_mhz: read_cpu_freq(),
        mem_total_kb: meminfo_value("MemTotal"),
        mem_available_kb: meminfo_value("MemAvailable"),
        load_average: read_trimmed("/proc/loadavg")
            .map(|v| v.split_whitespace().take(3).collect::<Vec<_>>().join(" ")),
    }
}

pub fn collect_capabilities(paths: &AppPaths) -> CapabilityReport {
    let cqhci = list_devices().ok().and_then(|devices| {
        devices
            .into_iter()
            .find(|device| device.name.starts_with("mmcblk") && !device.is_partition)
            .map(|device| detect_cqhci(&device.path))
    });
    CapabilityReport {
        io_uring_available: io_uring_available(),
        direct_io_available: direct_io_viable(paths).unwrap_or(false),
        deep_trace_available: deep_trace_available(),
        mmc_health_available: mmc_health_available(),
        procfs_access: Path::new("/proc/1").exists(),
        permission_level: if unsafe { libc::geteuid() } == 0 {
            "root".to_string()
        } else {
            format!("uid={}", unsafe { libc::geteuid() })
        },
        cqhci,
    }
}

pub fn doctor(paths: &AppPaths) -> DoctorReport {
    let caps = collect_capabilities(paths);
    let devices = list_devices().unwrap_or_default();
    let first_mmc = devices
        .iter()
        .find(|device| device.name.starts_with("mmcblk") && !device.is_partition);
    let first_mmc_name = first_mmc.map(|device| device.name.as_str());
    let first_mmc_path = first_mmc.map(|device| device.path.as_path());
    let mut checks = Vec::new();
    checks.push(
        if let Some(kernel) = collect_system_snapshot().kernel_version {
            pass_check("kernel_version", kernel)
        } else {
            fail_check("kernel_version", "unavailable")
        },
    );
    checks.push(if caps.io_uring_available {
        pass_check("io_uring_availability", "io_uring appears available")
    } else {
        warn_check(
            "io_uring_availability",
            "io_uring unavailable or disabled; fallback backend will be used",
        )
    });
    checks.push(if caps.direct_io_available {
        pass_check("o_direct_viability", "O_DIRECT test succeeded")
    } else {
        warn_check(
            "o_direct_viability",
            "O_DIRECT test failed or filesystem rejected it",
        )
    });
    checks.push(if fs::metadata(&paths.sessions_dir).is_ok() {
        pass_check(
            "writable_output_dir",
            paths.sessions_dir.display().to_string(),
        )
    } else {
        fail_check(
            "writable_output_dir",
            paths.sessions_dir.display().to_string(),
        )
    });
    checks.push(if caps.mmc_health_available {
        pass_check("mmc_utils", "MMC health path available")
    } else {
        warn_check(
            "mmc_utils",
            "no EXT_CSD path detected; mmc-utils and raw Linux MMC access are unavailable",
        )
    });
    checks.push(if caps.deep_trace_available {
        pass_check("deep_trace", "deep trace prerequisites detected")
    } else {
        warn_check(
            "deep_trace",
            "deep trace unavailable; root and tracing facilities are required",
        )
    });
    checks.push(if caps.procfs_access {
        pass_check("procfs_access", "/proc access available")
    } else {
        fail_check("procfs_access", "/proc access unavailable")
    });
    if let Some(mmc) = first_mmc_name {
        checks.push(boot_partition_presence_check(mmc));
        checks.push(boot_partition_force_ro_check(mmc));
    } else {
        checks.push(warn_check(
            "boot_partition_presence",
            "no mmcblk device found — boot partition checks skipped",
        ));
    }
    checks.push(cqhci_doctor_check(first_mmc_path));
    checks.push(mmc_host_state_check(first_mmc_path));
    checks.push(fiemap_support_check());
    checks.push(partition_start_check(first_mmc_name));
    checks.push(blktrace_support_check(first_mmc_name));
    checks.push(ftl_transparency_note());
    DoctorReport {
        generated_at: Utc::now(),
        checks,
        devices,
        capabilities: caps,
    }
}

pub fn detect_cqhci(device: &Path) -> CqhciStatus {
    let kernel_version =
        read_trimmed("/proc/sys/kernel/osrelease").unwrap_or_else(|| "unknown".to_string());
    let Some(mmc_host) = resolve_mmc_host(device) else {
        return CqhciStatus {
            mmc_host: "unknown".to_string(),
            cmdq_enabled: false,
            cmdq_depth: None,
            driver_name: None,
            kernel_version,
            risk: CqhciRisk::None,
            warning: None,
        };
    };
    let host_dir = PathBuf::from("/sys/class/mmc_host").join(&mmc_host);
    let mut driver_name = read_link_basename(host_dir.join("device/driver"));
    if driver_name.is_none() && Path::new("/sys/bus/platform/drivers/sdhci-esdhc-imx").exists() {
        driver_name = Some("sdhci-esdhc-imx".to_string());
    }
    let cmdq_enabled = read_trimmed(host_dir.join("cmdq_enabled"))
        .and_then(|value| parse_bool_flag(&value))
        .unwrap_or(false);
    let cmdq_depth = read_trimmed(host_dir.join("device/cmdq_depth"))
        .or_else(|| read_trimmed(host_dir.join("cmdq_depth")))
        .and_then(|value| value.parse::<u8>().ok());
    let risk = classify_risk(&driver_name, &kernel_version, cmdq_enabled);
    let warning = if risk == CqhciRisk::High {
        Some(format!(
            "CQHCI is enabled on {mmc_host} with driver {} and kernel {kernel_version}; NXP i.MX kernels before 5.15 are high-risk for CMDQ issues.",
            driver_name.as_deref().unwrap_or("unknown")
        ))
    } else {
        None
    };
    CqhciStatus {
        mmc_host,
        cmdq_enabled,
        cmdq_depth,
        driver_name,
        kernel_version,
        risk,
        warning,
    }
}

pub fn read_mmc_host_state(device: &Path) -> Result<MmcHostState> {
    let base_name = normalize_mmc_block_name(device).ok_or_else(|| {
        anyhow!(
            "unable to normalize MMC block name for {}",
            device.display()
        )
    })?;
    let mmc_host = resolve_mmc_host(device)
        .ok_or_else(|| anyhow!("unable to resolve mmc_host for {}", device.display()))?;
    let host_dir = PathBuf::from("/sys/class/mmc_host").join(&mmc_host);
    let block_dir = PathBuf::from("/sys/class/block").join(&base_name);
    let ios = read_trimmed(host_dir.join("ios"));
    let (ios_clock_hz, ios_bus_width, ios_timing) = parse_ios_snapshot(ios.as_deref());
    let card_state = read_trimmed(block_dir.join("device/state"));
    let card_type = read_trimmed(block_dir.join("device/type"));
    let ocr = read_trimmed(host_dir.join("device/ocr"))
        .and_then(|value| parse_sysfs_hex_u64(&value))
        .map(|value| value as u32);
    let cmdq_enabled =
        read_trimmed(host_dir.join("cmdq_enabled")).and_then(|value| parse_bool_flag(&value));
    Ok(MmcHostState {
        mmc_host,
        card_present: block_dir.join("device").exists()
            && card_state.as_deref() != Some("disconnected"),
        card_type,
        card_state,
        ios_clock_hz,
        ios_bus_width,
        ios_timing,
        ocr,
        cmdq_enabled,
        read_at: Utc::now(),
    })
}

pub fn decode_ocr(ocr: u32) -> String {
    let ready = (ocr >> 31) & 1 == 1;
    let hcs = (ocr >> 30) & 1 == 1;
    format!("0x{:08X}  ready={}  hcs={}", ocr, ready, hcs)
}

pub fn list_devices() -> Result<Vec<DeviceInfo>> {
    if !Path::new("/sys/class/block").exists() {
        return Ok(Vec::new());
    }
    let mounts = parse_mountinfo()?;
    let root_source = mounts
        .iter()
        .find(|m| m.mountpoint == "/")
        .map(|m| m.source.clone());
    let mut devices = Vec::new();
    for entry in fs::read_dir("/sys/class/block").context("read /sys/class/block")? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();
        let dev_path = PathBuf::from("/dev").join(&name);
        let sys_path = entry.path();
        let is_partition = sys_path.join("partition").exists();
        let parent = partition_parent_name(&sys_path, is_partition);
        let model = read_trimmed(entry.path().join("device/model"));
        let media_type = read_trimmed(entry.path().join("device/type"));
        let size_bytes = read_trimmed(entry.path().join("size"))
            .and_then(|s| s.parse::<u64>().ok())
            .map(|sectors| sectors.saturating_mul(512));
        let removable =
            read_trimmed(entry.path().join("removable")).and_then(|s| match s.as_str() {
                "0" => Some(false),
                "1" => Some(true),
                _ => None,
            });
        let logical_block_size = read_trimmed(entry.path().join("queue/logical_block_size"))
            .and_then(|s| s.parse::<u64>().ok());
        let major_minor = read_trimmed(entry.path().join("dev")).unwrap_or_default();
        let mut mountpoints = Vec::new();
        let mut filesystem_types = Vec::new();
        let mut mount_options = Vec::new();
        for mount in mounts
            .iter()
            .filter(|m| m.source == dev_path.display().to_string() || m.major_minor == major_minor)
        {
            mountpoints.push(mount.mountpoint.clone());
            filesystem_types.push(mount.fs_type.clone());
            mount_options.push(mount.mount_options.clone());
        }
        devices.push(DeviceInfo {
            path: dev_path.clone(),
            name,
            parent,
            is_partition,
            media_type,
            model,
            size_bytes,
            removable,
            mounted: !mountpoints.is_empty(),
            mountpoints,
            filesystem_types,
            mount_options,
            is_root_device: root_source
                .as_ref()
                .map(|root| root == &dev_path.display().to_string())
                .unwrap_or(false),
            logical_block_size,
        });
    }
    devices.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(devices)
}

fn partition_parent_name(sys_path: &Path, is_partition: bool) -> Option<String> {
    if !is_partition {
        return None;
    }
    let canonical = fs::canonicalize(sys_path).ok()?;
    let parent = canonical.parent()?;
    let name = parent.file_name()?.to_string_lossy().to_string();
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

pub fn assess_raw_target_safety(
    path: &Path,
    write_intent: bool,
    confirmed: bool,
) -> Result<SafetyAssessment> {
    let mut assessment = SafetyAssessment::default();
    let metadata =
        fs::metadata(path).with_context(|| format!("failed to stat {}", path.display()))?;
    let file_type = metadata.file_type();
    if !file_type.is_block_device() {
        assessment
            .reasons
            .push("target is not a block device".to_string());
    }
    let mounts = parse_mountinfo().unwrap_or_default();
    let path_string = path.display().to_string();
    let mounted = mounts.iter().any(|m| m.source == path_string);
    if mounted {
        assessment.reasons.push("target is mounted".to_string());
    }
    if mounts
        .iter()
        .find(|m| m.mountpoint == "/")
        .map(|m| m.source == path_string)
        .unwrap_or(false)
    {
        assessment
            .reasons
            .push("target is the active root device".to_string());
    }
    if write_intent && !confirmed {
        assessment.reasons.push(
            "missing destructive confirmation flag --i-understand-this-will-destroy-data"
                .to_string(),
        );
    }
    if !write_intent {
        assessment.warnings.push(
            "raw read workloads are non-destructive but still bypass filesystem safeguards"
                .to_string(),
        );
    }
    assessment.allowed = assessment.reasons.is_empty();
    Ok(assessment)
}

pub fn block_device_size(path: &Path) -> Option<u64> {
    let name = path.file_name()?.to_string_lossy().to_string();
    read_trimmed(PathBuf::from("/sys/class/block").join(name).join("size"))
        .and_then(|s| s.parse::<u64>().ok())
        .map(|sectors| sectors.saturating_mul(512))
}

pub fn logical_block_size(path: &Path) -> Option<u64> {
    let name = path.file_name()?.to_string_lossy().to_string();
    read_trimmed(
        PathBuf::from("/sys/class/block")
            .join(name)
            .join("queue/logical_block_size"),
    )
    .and_then(|s| s.parse::<u64>().ok())
}

pub fn command_exists(name: &str) -> bool {
    let path_var = match env::var("PATH") {
        Ok(value) => value,
        Err(_) => return false,
    };
    path_var
        .split(':')
        .map(PathBuf::from)
        .any(|path| path.join(name).exists())
}

fn mmc_health_available() -> bool {
    #[cfg(target_os = "linux")]
    {
        command_exists("mmc")
            || fs::read_dir("/sys/class/block")
                .ok()
                .into_iter()
                .flat_map(|entries| entries.flatten())
                .any(|entry| entry.file_name().to_string_lossy().starts_with("mmcblk"))
    }
    #[cfg(not(target_os = "linux"))]
    {
        command_exists("mmc")
    }
}

pub fn io_uring_available() -> bool {
    let disabled = read_trimmed("/proc/sys/kernel/io_uring_disabled")
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);
    disabled != 2 && read_trimmed("/proc/sys/kernel/osrelease").is_some()
}

pub fn deep_trace_available() -> bool {
    let is_root = unsafe { libc::geteuid() } == 0;
    let tracing = Path::new("/sys/kernel/tracing").exists()
        || Path::new("/sys/kernel/debug/tracing").exists();
    let bpf = Path::new("/sys/fs/bpf").exists();
    is_root && (tracing || bpf)
}

pub fn direct_io_viable(paths: &AppPaths) -> Result<bool> {
    let path = paths.base_dir.join("o_direct_probe.bin");
    fs::write(&path, vec![0_u8; 4096])?;
    let c_path = std::ffi::CString::new(path.to_string_lossy().to_string())?;
    let fd = unsafe { libc::open(c_path.as_ptr(), libc::O_RDWR | o_direct_flag(), 0o644) };
    let ok = if fd < 0 {
        false
    } else {
        let mut ptr: *mut libc::c_void = std::ptr::null_mut();
        let aligned = unsafe { libc::posix_memalign(&mut ptr, 4096, 4096) };
        if aligned != 0 {
            false
        } else {
            unsafe { std::ptr::write_bytes(ptr, 0, 4096) };
            let wrote = unsafe { libc::pwrite(fd, ptr, 4096, 0) };
            unsafe {
                libc::free(ptr);
            }
            wrote == 4096
        }
    };
    close_fd(fd);
    let _ = fs::remove_file(path);
    Ok(ok)
}

pub fn resolve_uid_to_user(uid: u32) -> String {
    let passwd = fs::read_to_string("/etc/passwd").unwrap_or_default();
    for line in passwd.lines() {
        let parts: Vec<&str> = line.split(':').collect();
        if parts.len() > 2 {
            if parts[2].parse::<u32>().ok() == Some(uid) {
                return parts[0].to_string();
            }
        }
    }
    uid.to_string()
}

pub fn read_link_string(path: impl AsRef<Path>) -> Option<String> {
    fs::read_link(path).ok().map(|p| p.display().to_string())
}

fn parse_mountinfo() -> Result<Vec<MountInfo>> {
    let Ok(text) = fs::read_to_string("/proc/self/mountinfo") else {
        return Ok(Vec::new());
    };
    let mut mounts = Vec::new();
    for line in text.lines() {
        let Some((left, right)) = line.split_once(" - ") else {
            continue;
        };
        let left_parts: Vec<&str> = left.split_whitespace().collect();
        let right_parts: Vec<&str> = right.split_whitespace().collect();
        if left_parts.len() < 6 || right_parts.len() < 3 {
            continue;
        }
        let merged_options = if right_parts[2].is_empty() {
            left_parts[5].to_string()
        } else {
            format!("{},{}", left_parts[5], right_parts[2])
        };
        mounts.push(MountInfo {
            major_minor: left_parts[2].to_string(),
            mountpoint: left_parts[4].to_string(),
            fs_type: right_parts[0].to_string(),
            mount_options: merged_options,
            source: right_parts[1].to_string(),
        });
    }
    Ok(mounts)
}

fn meminfo_value(name: &str) -> Option<u64> {
    let text = fs::read_to_string("/proc/meminfo").ok()?;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix(name) {
            let value = rest
                .split_whitespace()
                .find_map(|part| part.parse::<u64>().ok());
            if value.is_some() {
                return value;
            }
        }
    }
    None
}

fn read_cpu_temp() -> Option<f32> {
    let candidates = [
        "/sys/class/thermal/thermal_zone0/temp",
        "/sys/class/hwmon/hwmon0/temp1_input",
    ];
    for path in candidates {
        if let Some(raw) = read_trimmed(path).and_then(|v| v.parse::<f32>().ok()) {
            return Some(if raw > 1000.0 { raw / 1000.0 } else { raw });
        }
    }
    None
}

fn read_cpu_freq() -> Option<u64> {
    let candidates = [
        "/sys/devices/system/cpu/cpu0/cpufreq/scaling_cur_freq",
        "/sys/devices/system/cpu/cpu0/cpufreq/cpuinfo_cur_freq",
    ];
    for path in candidates {
        if let Some(raw) = read_trimmed(path).and_then(|v| v.parse::<u64>().ok()) {
            return Some(raw / 1000);
        }
    }
    None
}

fn boot_partition_presence_check(parent_name: &str) -> DoctorCheck {
    let boot_partitions = list_boot_partitions(parent_name);
    if boot_partitions.len() == 2 {
        pass_check(
            "boot_partition_presence",
            format!(
                "{} present",
                boot_partitions
                    .iter()
                    .filter_map(|path| {
                        path.file_name()
                            .map(|name| name.to_string_lossy().to_string())
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        )
    } else {
        warn_check(
            "boot_partition_presence",
            if boot_partitions.is_empty() {
                format!("no boot partitions found for {parent_name} under /sys/class/block")
            } else {
                format!(
                    "{} present",
                    boot_partitions
                        .iter()
                        .filter_map(|path| path
                            .file_name()
                            .map(|name| name.to_string_lossy().to_string()))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            },
        )
    }
}

fn boot_partition_force_ro_check(parent_name: &str) -> DoctorCheck {
    let boot_partitions = list_boot_partitions(parent_name);
    let mut states = Vec::new();
    for path in boot_partitions {
        let Some(name) = path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
        else {
            continue;
        };
        let state = read_trimmed(Path::new("/sys/class/block").join(&name).join("force_ro"))
            .unwrap_or_else(|| "unavailable".to_string());
        states.push(format!("{name}={state}"));
    }
    if states.is_empty() {
        warn_check(
            "boot_partition_force_ro",
            format!("unable to read force_ro for boot partitions under {parent_name}"),
        )
    } else {
        pass_check("boot_partition_force_ro", states.join(", "))
    }
}

pub(crate) fn read_trimmed(path: impl AsRef<Path>) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn fiemap_support_check() -> DoctorCheck {
    #[cfg(target_os = "linux")]
    {
        match fiemap_support_check_linux() {
            Ok(check) => check,
            Err(err) => fail_check("fiemap_support", format!("FIEMAP probe failed: {err:#}")),
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        fail_check("fiemap_support", "FIEMAP is Linux-only")
    }
}

fn partition_start_check(device: Option<&str>) -> DoctorCheck {
    #[cfg(target_os = "linux")]
    {
        let Some(device) = device else {
            return warn_check(
                "partition_start_lba",
                "no mmcblk device found under /sys/class/block",
            );
        };
        let base = PathBuf::from("/sys/class/block").join(device);
        let Ok(entries) = fs::read_dir(&base) else {
            return fail_check(
                "partition_start_lba",
                format!("unable to inspect {}", base.display()),
            );
        };
        let mut starts = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.starts_with(device) || name == device {
                continue;
            }
            if let Some(start) = read_trimmed(entry.path().join("start")) {
                starts.push(format!("{name}={start}"));
            }
        }
        if starts.is_empty() {
            warn_check(
                "partition_start_lba",
                format!("no readable partition starts under {}", base.display()),
            )
        } else {
            pass_check("partition_start_lba", starts.join(", "))
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = device;
        fail_check(
            "partition_start_lba",
            "partition start probing is Linux-only",
        )
    }
}

fn ftl_transparency_note() -> DoctorCheck {
    pass_check(
        "ftl_transparency",
        "All host-side addresses are logical LBAs, not physical NAND locations. This is a fundamental constraint of all userspace eMMC tooling.",
    )
}

fn blktrace_support_check(device: Option<&str>) -> DoctorCheck {
    #[cfg(target_os = "linux")]
    {
        let debugfs = Path::new("/sys/kernel/debug/block");
        let has_blktrace = command_exists("blktrace");
        let detail = if !has_blktrace {
            "install blktrace to enable block-layer trace capture".to_string()
        } else if !debugfs.exists() {
            "mount debugfs at /sys/kernel/debug to inspect block trace support".to_string()
        } else if let Some(device) = device {
            let trace_dir = debugfs.join(device);
            if trace_dir.exists() {
                format!("available for {device}; requires debugfs access and root")
            } else {
                format!(
                    "debugfs is mounted, but {} is missing; kernel trace hooks may be disabled",
                    trace_dir.display()
                )
            }
        } else {
            "debugfs is mounted and blktrace is installed".to_string()
        };
        let ok = has_blktrace
            && debugfs.exists()
            && device
                .map(|name| debugfs.join(name).exists())
                .unwrap_or(true);
        if ok {
            pass_check("blktrace_support", detail)
        } else {
            warn_check("blktrace_support", detail)
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = device;
        fail_check("blktrace_support", "blktrace capture is Linux-only")
    }
}

#[cfg(target_os = "linux")]
fn fiemap_support_check_linux() -> Result<DoctorCheck> {
    let probe_path = env::temp_dir().join(format!("emmc-lab-fiemap-probe-{}", process::id()));
    fs::write(&probe_path, b"probe")
        .with_context(|| format!("failed to create {}", probe_path.display()))?;
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&probe_path)
        .with_context(|| format!("failed to open {}", probe_path.display()))?;
    let mut request = FiemapProbe {
        fm_start: 0,
        fm_length: !0_u64,
        fm_flags: 0,
        fm_mapped_extents: 0,
        fm_extent_count: 0,
        fm_reserved: 0,
    };
    let rc = unsafe { libc::ioctl(file.as_raw_fd(), FS_IOC_FIEMAP, &mut request) };
    let _ = fs::remove_file(&probe_path);
    if rc == 0 {
        return Ok(pass_check(
            "fiemap_support",
            "FIEMAP ioctl accepted on the current filesystem",
        ));
    }
    let err = io::Error::last_os_error();
    let detail = if err.raw_os_error() == Some(libc::EOPNOTSUPP) {
        "current filesystem returned EOPNOTSUPP for FIEMAP".to_string()
    } else {
        format!("FIEMAP ioctl failed: {err}")
    };
    Ok(warn_check("fiemap_support", detail))
}

fn cqhci_doctor_check(device: Option<&Path>) -> DoctorCheck {
    let Some(device) = device else {
        return warn_check("cqhci_status", "no mmcblk device found for CQHCI detection");
    };
    let status = detect_cqhci(device);
    let detail = format!(
        "host={} enabled={} depth={} driver={} kernel={} risk={:?}",
        status.mmc_host,
        status.cmdq_enabled,
        status
            .cmdq_depth
            .map(|value| value.to_string())
            .unwrap_or_else(|| "-".to_string()),
        status.driver_name.as_deref().unwrap_or("-"),
        status.kernel_version,
        status.risk
    );
    match status.risk {
        CqhciRisk::High => warn_check("cqhci_status", detail),
        CqhciRisk::Low | CqhciRisk::None => pass_check("cqhci_status", detail),
    }
}

fn mmc_host_state_check(device: Option<&Path>) -> DoctorCheck {
    let Some(device) = device else {
        return warn_check(
            "mmc_host_state",
            "no mmcblk device found for host-state snapshot",
        );
    };
    match read_mmc_host_state(device) {
        Ok(state) => {
            let detail = format!(
                "host={} state={} clock={} cmdq={}",
                state.mmc_host,
                state.card_state.as_deref().unwrap_or("-"),
                state
                    .ios_clock_hz
                    .map(|value| format!("{value} Hz"))
                    .unwrap_or_else(|| "-".to_string()),
                state
                    .cmdq_enabled
                    .map(|value| if value { "enabled" } else { "disabled" }.to_string())
                    .unwrap_or_else(|| "-".to_string())
            );
            if state.card_present {
                pass_check("mmc_host_state", detail)
            } else {
                warn_check("mmc_host_state", detail)
            }
        }
        Err(err) => warn_check("mmc_host_state", format!("{err:#}")),
    }
}

fn pass_check(name: impl Into<String>, detail: impl Into<String>) -> DoctorCheck {
    DoctorCheck {
        name: name.into(),
        status: DoctorStatus::Pass,
        detail: detail.into(),
    }
}

fn warn_check(name: impl Into<String>, detail: impl Into<String>) -> DoctorCheck {
    DoctorCheck {
        name: name.into(),
        status: DoctorStatus::Warn,
        detail: detail.into(),
    }
}

fn fail_check(name: impl Into<String>, detail: impl Into<String>) -> DoctorCheck {
    DoctorCheck {
        name: name.into(),
        status: DoctorStatus::Fail,
        detail: detail.into(),
    }
}

fn resolve_mmc_host(device: &Path) -> Option<String> {
    let device_name = device.file_name()?.to_string_lossy().to_string();
    let sys_device = fs::canonicalize(
        Path::new("/sys/class/block")
            .join(&device_name)
            .join("device"),
    )
    .ok()?;
    for ancestor in sys_device.ancestors() {
        let name = ancestor.file_name()?.to_string_lossy();
        if let Some(rest) = name.strip_prefix("mmc") {
            if !rest.is_empty() && rest.chars().all(|ch| ch.is_ascii_digit()) {
                return Some(name.to_string());
            }
        }
    }
    for entry in fs::read_dir("/sys/class/mmc_host").ok()?.flatten() {
        let host = entry.file_name().to_string_lossy().to_string();
        if let Ok(host_path) = fs::canonicalize(entry.path()) {
            if sys_device.starts_with(&host_path) {
                return Some(host);
            }
        }
    }
    None
}

fn normalize_mmc_block_name(device: &Path) -> Option<String> {
    let name = device.file_name()?.to_string_lossy();
    if let Some((base, suffix)) = name.rsplit_once('p') {
        if base.starts_with("mmcblk") && suffix.chars().all(|ch| ch.is_ascii_digit()) {
            return Some(base.to_string());
        }
    }
    if let Some((base, suffix)) = name.split_once("boot") {
        if base.starts_with("mmcblk") && suffix.chars().all(|ch| ch.is_ascii_digit()) {
            return Some(base.to_string());
        }
    }
    Some(name.to_string())
}

fn parse_bool_flag(value: &str) -> Option<bool> {
    match value.trim() {
        "0" => Some(false),
        "1" => Some(true),
        _ => None,
    }
}

fn parse_sysfs_hex_u64(value: &str) -> Option<u64> {
    value
        .trim()
        .trim_start_matches("0x")
        .trim_start_matches("0X")
        .parse::<u64>()
        .ok()
        .or_else(|| u64::from_str_radix(value.trim().trim_start_matches("0x"), 16).ok())
}

fn read_link_basename(path: impl AsRef<Path>) -> Option<String> {
    fs::read_link(path)
        .ok()?
        .file_name()
        .map(|value| value.to_string_lossy().to_string())
}

fn classify_risk(driver: &Option<String>, kernel: &str, enabled: bool) -> CqhciRisk {
    if !enabled {
        return CqhciRisk::None;
    }
    let is_nxp = driver
        .as_deref()
        .map(|d| d.contains("esdhc") || d.contains("usdhc"))
        .unwrap_or(false);
    if !is_nxp {
        return CqhciRisk::Low;
    }
    let risky_kernel = parse_kernel_minor(kernel)
        .map(|(maj, min)| maj == 5 && min < 15)
        .unwrap_or(false);
    if risky_kernel {
        CqhciRisk::High
    } else {
        CqhciRisk::Low
    }
}

fn parse_kernel_minor(kernel: &str) -> Option<(u32, u32)> {
    let mut parts = kernel.split('.');
    let major = parts.next()?.parse::<u32>().ok()?;
    let minor_text = parts.next()?;
    let minor_digits = minor_text
        .chars()
        .take_while(|ch| ch.is_ascii_digit())
        .collect::<String>();
    let minor = minor_digits.parse::<u32>().ok()?;
    Some((major, minor))
}

fn parse_ios_snapshot(ios: Option<&str>) -> (Option<u64>, Option<u8>, Option<String>) {
    let Some(ios) = ios else {
        return (None, None, None);
    };
    let mut clock = None;
    let mut bus_width = None;
    let mut timing = None;
    for line in ios.lines() {
        let trimmed = line.trim();
        if let Some(value) = trimmed.strip_prefix("clock:") {
            clock = value
                .split_whitespace()
                .find_map(|part| part.parse::<u64>().ok());
        } else if let Some(value) = trimmed.strip_prefix("bus width:") {
            bus_width = value
                .split('(')
                .nth(1)
                .and_then(|rest| rest.split_whitespace().next())
                .and_then(|digits| digits.parse::<u8>().ok())
                .or_else(|| {
                    value
                        .split_whitespace()
                        .next()
                        .and_then(|digits| digits.parse::<u8>().ok())
                });
        } else if let Some(value) = trimmed.strip_prefix("timing spec:") {
            timing = Some(value.trim().to_ascii_lowercase());
        }
    }
    (clock, bus_width, timing)
}

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
struct FiemapProbe {
    fm_start: u64,
    fm_length: u64,
    fm_flags: u32,
    fm_mapped_extents: u32,
    fm_extent_count: u32,
    fm_reserved: u32,
}

#[cfg(target_os = "linux")]
const FS_IOC_FIEMAP: libc::c_ulong =
    iowr(b'f' as u32, 11, std::mem::size_of::<FiemapProbe>() as u32);

fn close_fd(fd: RawFd) {
    if fd >= 0 {
        unsafe {
            libc::close(fd);
        }
    }
}

#[allow(dead_code)]
fn _io_error(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::Other, message.into())
}

#[allow(dead_code)]
fn _ensure_path_exists(path: &Path) -> Result<()> {
    if path.exists() {
        Ok(())
    } else {
        Err(anyhow!("{} does not exist", path.display()))
    }
}

pub fn diskstats_map() -> HashMap<String, Vec<u64>> {
    let mut map = HashMap::new();
    let text = fs::read_to_string("/proc/diskstats").unwrap_or_default();
    for line in text.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 14 {
            continue;
        }
        let name = parts[2].to_string();
        let values = parts[3..]
            .iter()
            .filter_map(|v| v.parse::<u64>().ok())
            .collect::<Vec<_>>();
        map.insert(name, values);
    }
    map
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
