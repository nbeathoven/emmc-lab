use crate::system::{collect_capabilities, collect_system_snapshot, command_exists, AppPaths, SystemSnapshot};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EmmcHealthSnapshot {
    pub collected_at: DateTime<Utc>,
    pub device: PathBuf,
    pub available: bool,
    pub device_life_time_est_typ_a: Option<String>,
    pub device_life_time_est_typ_b: Option<String>,
    pub pre_eol_info: Option<String>,
    pub raw_text: Option<String>,
    pub note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthReport {
    pub system: SystemSnapshot,
    pub capabilities: crate::system::CapabilityReport,
    pub emmc: EmmcHealthSnapshot,
}

pub fn collect_health(paths: &AppPaths, device: &Path) -> Result<HealthReport> {
    Ok(HealthReport {
        system: collect_system_snapshot(),
        capabilities: collect_capabilities(paths),
        emmc: read_emmc_health(device)?,
    })
}

pub fn read_emmc_health(device: &Path) -> Result<EmmcHealthSnapshot> {
    let now = Utc::now();
    if !command_exists("mmc") {
        return Ok(EmmcHealthSnapshot {
            collected_at: now,
            device: device.to_path_buf(),
            available: false,
            note: "health telemetry unavailable: mmc-utils not found".to_string(),
            ..EmmcHealthSnapshot::default()
        });
    }

    let output = Command::new("mmc")
        .arg("extcsd")
        .arg("read")
        .arg(device)
        .output()
        .context("failed to execute mmc-utils")?;

    let raw_stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let raw_stderr = String::from_utf8_lossy(&output.stderr).to_string();
    if !output.status.success() {
        return Ok(EmmcHealthSnapshot {
            collected_at: now,
            device: device.to_path_buf(),
            available: false,
            raw_text: Some(format!("{}\n{}", raw_stdout, raw_stderr).trim().to_string()),
            note: "health telemetry unavailable: mmc-utils command failed".to_string(),
            ..EmmcHealthSnapshot::default()
        });
    }

    let combined = raw_stdout.trim().to_string();
    Ok(EmmcHealthSnapshot {
        collected_at: now,
        device: device.to_path_buf(),
        available: true,
        device_life_time_est_typ_a: parse_extcsd_field(&combined, "DEVICE_LIFE_TIME_EST_TYP_A"),
        device_life_time_est_typ_b: parse_extcsd_field(&combined, "DEVICE_LIFE_TIME_EST_TYP_B"),
        pre_eol_info: parse_extcsd_field(&combined, "PRE_EOL_INFO"),
        raw_text: Some(combined),
        note: "health values are best-effort values reported by EXT_CSD when mmc-utils is available".to_string(),
    })
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

#[cfg(test)]
mod tests {
    use super::parse_extcsd_field;

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
}
