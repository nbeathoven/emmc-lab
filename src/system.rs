use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::env;
use std::fs;
use std::io;
use std::os::fd::RawFd;
use std::os::unix::fs::FileTypeExt;
use std::path::{Path, PathBuf};

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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoctorCheck {
    pub name: String,
    pub ok: bool,
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
    CapabilityReport {
        io_uring_available: io_uring_available(),
        direct_io_available: direct_io_viable(paths).unwrap_or(false),
        deep_trace_available: deep_trace_available(),
        mmc_health_available: command_exists("mmc"),
        procfs_access: Path::new("/proc/1").exists(),
        permission_level: if unsafe { libc::geteuid() } == 0 {
            "root".to_string()
        } else {
            format!("uid={}", unsafe { libc::geteuid() })
        },
    }
}

pub fn doctor(paths: &AppPaths) -> DoctorReport {
    let caps = collect_capabilities(paths);
    let devices = list_devices().unwrap_or_default();
    let mut checks = Vec::new();
    checks.push(DoctorCheck {
        name: "kernel_version".to_string(),
        ok: collect_system_snapshot().kernel_version.is_some(),
        detail: collect_system_snapshot()
            .kernel_version
            .unwrap_or_else(|| "unavailable".to_string()),
    });
    checks.push(DoctorCheck {
        name: "io_uring_availability".to_string(),
        ok: caps.io_uring_available,
        detail: if caps.io_uring_available {
            "io_uring appears available".to_string()
        } else {
            "io_uring unavailable or disabled; fallback backend will be used".to_string()
        },
    });
    checks.push(DoctorCheck {
        name: "o_direct_viability".to_string(),
        ok: caps.direct_io_available,
        detail: if caps.direct_io_available {
            "O_DIRECT test succeeded".to_string()
        } else {
            "O_DIRECT test failed or filesystem rejected it".to_string()
        },
    });
    checks.push(DoctorCheck {
        name: "writable_output_dir".to_string(),
        ok: fs::metadata(&paths.sessions_dir).is_ok(),
        detail: paths.sessions_dir.display().to_string(),
    });
    checks.push(DoctorCheck {
        name: "mmc_utils".to_string(),
        ok: caps.mmc_health_available,
        detail: if caps.mmc_health_available {
            "mmc-utils available".to_string()
        } else {
            "mmc-utils missing; health telemetry will be unavailable".to_string()
        },
    });
    checks.push(DoctorCheck {
        name: "deep_trace".to_string(),
        ok: caps.deep_trace_available,
        detail: if caps.deep_trace_available {
            "deep trace prerequisites detected".to_string()
        } else {
            "deep trace unavailable; root and tracing facilities are required".to_string()
        },
    });
    checks.push(DoctorCheck {
        name: "procfs_access".to_string(),
        ok: caps.procfs_access,
        detail: if caps.procfs_access {
            "/proc access available".to_string()
        } else {
            "/proc access unavailable".to_string()
        },
    });
    DoctorReport {
        generated_at: Utc::now(),
        checks,
        devices,
        capabilities: caps,
    }
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

fn read_trimmed(path: impl AsRef<Path>) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

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
