use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::fmt::{Display, Formatter};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TargetMode {
    RawDevice,
    FileBased,
}

impl Display for TargetMode {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RawDevice => write!(f, "raw_device"),
            Self::FileBased => write!(f, "file_based"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum WorkloadType {
    #[serde(rename = "randread", alias = "rand_read")]
    RandRead,
    #[serde(rename = "randwrite", alias = "rand_write")]
    RandWrite,
    #[serde(rename = "randrw", alias = "rand_rw")]
    RandRw,
    #[serde(rename = "read")]
    Read,
    #[serde(rename = "write")]
    Write,
}

impl Display for WorkloadType {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RandRead => write!(f, "randread"),
            Self::RandWrite => write!(f, "randwrite"),
            Self::RandRw => write!(f, "randrw"),
            Self::Read => write!(f, "read"),
            Self::Write => write!(f, "write"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AddressingMode {
    WholeSelectedRange,
    SectorRange,
    ByteRange,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StopCondition {
    RuntimeSeconds,
    ExactOperationCount,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DurabilityMode {
    Performance,
    BatchDurable,
    StrictDurable,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TargetConfig {
    pub mode: TargetMode,
    pub path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkloadConfig {
    pub test_type: WorkloadType,
    pub block_size_bytes: usize,
    pub runtime_seconds: Option<u64>,
    pub exact_op_count: Option<u64>,
    pub queue_depth: u32,
    pub worker_count: usize,
    pub read_percentage: u8,
    pub random_seed: u64,
    pub direct_io: bool,
    pub verify: bool,
    pub sync_cadence_writes: Option<u64>,
    pub sync_cadence_ms: Option<u64>,
    pub report_interval_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AddressingConfig {
    pub mode: AddressingMode,
    pub start_sector: Option<u64>,
    pub sector_count: Option<u64>,
    pub start_offset_bytes: Option<u64>,
    pub range_size_bytes: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DurabilityConfig {
    pub mode: DurabilityMode,
    pub fdatasync_at_end: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VerificationConfig {
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TelemetryConfig {
    pub health_telemetry: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiagnosticsConfig {
    pub capture_during_test: bool,
    pub deep_trace: bool,
    pub sample_interval_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReportingConfig {
    pub export_json: bool,
    pub export_csv: bool,
    pub export_html: bool,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SafetyConfig {
    pub destructive_confirmation_required: bool,
    pub confirmation_flag: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Profile {
    pub name: String,
    pub target: TargetConfig,
    pub workload: WorkloadConfig,
    pub addressing: AddressingConfig,
    pub durability: DurabilityConfig,
    pub verification: VerificationConfig,
    pub telemetry: TelemetryConfig,
    pub diagnostics: DiagnosticsConfig,
    pub reporting: ReportingConfig,
    pub safety: SafetyConfig,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedRange {
    pub start_bytes: u64,
    pub length_bytes: u64,
    pub logical_sector_size: u64,
}

impl Profile {
    pub fn default_named(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            target: TargetConfig {
                mode: TargetMode::FileBased,
                path: PathBuf::from("/tmp/emmc-lab.bin"),
            },
            workload: WorkloadConfig {
                test_type: WorkloadType::RandRead,
                block_size_bytes: 4096,
                runtime_seconds: Some(30),
                exact_op_count: None,
                queue_depth: 1,
                worker_count: 1,
                read_percentage: 70,
                random_seed: 1,
                direct_io: false,
                verify: false,
                sync_cadence_writes: Some(1024),
                sync_cadence_ms: Some(1000),
                report_interval_ms: 1000,
            },
            addressing: AddressingConfig {
                mode: AddressingMode::WholeSelectedRange,
                start_sector: None,
                sector_count: None,
                start_offset_bytes: Some(0),
                range_size_bytes: None,
            },
            durability: DurabilityConfig {
                mode: DurabilityMode::Performance,
                fdatasync_at_end: true,
            },
            verification: VerificationConfig { enabled: false },
            telemetry: TelemetryConfig {
                health_telemetry: true,
            },
            diagnostics: DiagnosticsConfig {
                capture_during_test: false,
                deep_trace: false,
                sample_interval_ms: 1000,
            },
            reporting: ReportingConfig {
                export_json: true,
                export_csv: true,
                export_html: true,
                notes: vec![],
            },
            safety: SafetyConfig {
                destructive_confirmation_required: true,
                confirmation_flag: false,
            },
        }
    }

    pub fn load(path: &Path) -> Result<Self> {
        let text = fs::read_to_string(path)
            .with_context(|| format!("failed to read profile {}", path.display()))?;
        let profile = serde_yaml::from_str::<Self>(&text)
            .with_context(|| format!("failed to parse YAML {}", path.display()))?;
        profile.validate()?;
        Ok(profile)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        let text = serde_yaml::to_string(self)?;
        fs::write(path, text).with_context(|| format!("failed to write {}", path.display()))?;
        Ok(())
    }

    pub fn validate(&self) -> Result<()> {
        if self.name.trim().is_empty() {
            bail!("profile name cannot be empty");
        }
        if self.target.path.as_os_str().is_empty() {
            bail!("target path cannot be empty");
        }
        if self.target.mode == TargetMode::FileBased
            && self.target.path.exists()
            && fs::metadata(&self.target.path)
                .map(|metadata| metadata.is_dir())
                .unwrap_or(false)
        {
            bail!(
                "file-based target {} is a directory; provide a file path instead",
                self.target.path.display()
            );
        }
        if self.workload.block_size_bytes == 0 {
            bail!("block_size_bytes must be > 0");
        }
        if self.workload.queue_depth == 0 {
            bail!("queue_depth must be > 0");
        }
        if self.workload.worker_count == 0 {
            bail!("worker_count must be > 0");
        }
        if self.workload.read_percentage > 100 {
            bail!("read_percentage must be <= 100");
        }
        match (
            self.workload.runtime_seconds,
            self.workload.exact_op_count,
            self.stop_condition(),
        ) {
            (Some(_), None, StopCondition::RuntimeSeconds) => {}
            (None, Some(count), StopCondition::ExactOperationCount) if count > 0 => {}
            _ => bail!("exactly one stop condition must be configured"),
        }
        match self.addressing.mode {
            AddressingMode::WholeSelectedRange => {}
            AddressingMode::SectorRange => {
                let start = self
                    .addressing
                    .start_sector
                    .ok_or_else(|| anyhow!("start_sector required for sector range"))?;
                let count = self
                    .addressing
                    .sector_count
                    .ok_or_else(|| anyhow!("sector_count required for sector range"))?;
                if count == 0 {
                    bail!("sector_count must be > 0");
                }
                let _ = start
                    .checked_add(count)
                    .ok_or_else(|| anyhow!("sector range overflow"))?;
            }
            AddressingMode::ByteRange => {
                let _ = self
                    .addressing
                    .start_offset_bytes
                    .ok_or_else(|| anyhow!("start_offset_bytes required for byte range"))?;
                let count = self
                    .addressing
                    .range_size_bytes
                    .ok_or_else(|| anyhow!("range_size_bytes required for byte range"))?;
                if count == 0 {
                    bail!("range_size_bytes must be > 0");
                }
            }
        }
        Ok(())
    }

    pub fn stop_condition(&self) -> StopCondition {
        if self.workload.exact_op_count.is_some() {
            StopCondition::ExactOperationCount
        } else {
            StopCondition::RuntimeSeconds
        }
    }

    pub fn is_write_workload(&self) -> bool {
        matches!(
            self.workload.test_type,
            WorkloadType::RandWrite | WorkloadType::RandRw | WorkloadType::Write
        )
    }

    pub fn is_random_workload(&self) -> bool {
        matches!(
            self.workload.test_type,
            WorkloadType::RandRead | WorkloadType::RandWrite | WorkloadType::RandRw
        )
    }

    pub fn resolved_range(
        &self,
        target_len: u64,
        logical_sector_size: u64,
    ) -> Result<ResolvedRange> {
        let logical_sector_size = logical_sector_size.max(512);
        let (start_bytes, length_bytes) = match self.addressing.mode {
            AddressingMode::WholeSelectedRange => (0_u64, target_len),
            AddressingMode::SectorRange => {
                let start_sector = self.addressing.start_sector.unwrap_or(0);
                let sector_count = self.addressing.sector_count.unwrap_or(0);
                let start = start_sector
                    .checked_mul(logical_sector_size)
                    .ok_or_else(|| anyhow!("start_sector overflow"))?;
                let len = sector_count
                    .checked_mul(logical_sector_size)
                    .ok_or_else(|| anyhow!("sector_count overflow"))?;
                (start, len)
            }
            AddressingMode::ByteRange => (
                self.addressing.start_offset_bytes.unwrap_or(0),
                self.addressing.range_size_bytes.unwrap_or(target_len),
            ),
        };

        if length_bytes == 0 {
            bail!("targeted range is empty");
        }
        let end = start_bytes
            .checked_add(length_bytes)
            .ok_or_else(|| anyhow!("range overflow"))?;
        if end > target_len {
            bail!(
                "selected range exceeds target size: end={} target={}",
                end,
                target_len
            );
        }
        let block = self.workload.block_size_bytes as u64;
        if start_bytes % block != 0 {
            bail!(
                "start offset {} is not aligned to block size {}",
                start_bytes,
                block
            );
        }
        if length_bytes < block {
            bail!("range must be at least one block");
        }
        Ok(ResolvedRange {
            start_bytes,
            length_bytes,
            logical_sector_size,
        })
    }

    pub fn printable_summary(&self) -> String {
        let stop = match self.stop_condition() {
            StopCondition::RuntimeSeconds => format!(
                "runtime={}s",
                self.workload.runtime_seconds.unwrap_or_default()
            ),
            StopCondition::ExactOperationCount => format!(
                "exact_op_count={}",
                self.workload.exact_op_count.unwrap_or_default()
            ),
        };
        format!(
            concat!(
                "Profile: {name}\n",
                "Target mode: {target_mode}\n",
                "Target path: {target_path}\n",
                "Workload: {test_type}\n",
                "Addressing: {addressing:?}\n",
                "Block size: {block_size}\n",
                "Queue depth: {queue_depth}\n",
                "Workers: {workers}\n",
                "Seed: {seed}\n",
                "Direct I/O: {direct}\n",
                "Verify: {verify}\n",
                "Durability: {durability:?}\n",
                "Health telemetry: {health}\n",
                "Diagnostic capture: {diag}\n",
                "Stop condition: {stop}\n",
                "Note: logical sector/LBA targeting only; this does not guarantee fixed physical NAND cell targeting."
            ),
            name = self.name,
            target_mode = self.target.mode,
            target_path = self.target.path.display(),
            test_type = self.workload.test_type,
            addressing = self.addressing.mode,
            block_size = self.workload.block_size_bytes,
            queue_depth = self.workload.queue_depth,
            workers = self.workload.worker_count,
            seed = self.workload.random_seed,
            direct = self.workload.direct_io,
            verify = self.verification.enabled,
            durability = self.durability.mode,
            health = self.telemetry.health_telemetry,
            diag = self.diagnostics.capture_during_test,
            stop = stop,
        )
    }

    pub fn equivalent_command(&self, profile_path: &Path) -> String {
        format!("emmc-lab run --profile {}", profile_path.display())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_profile_yaml() {
        let yaml = r#"
name: smoke
target:
  mode: file_based
  path: /tmp/test.bin
workload:
  test_type: randwrite
  block_size_bytes: 4096
  runtime_seconds:
  exact_op_count: 100
  queue_depth: 1
  worker_count: 2
  read_percentage: 50
  random_seed: 7
  direct_io: false
  verify: true
  sync_cadence_writes: 8
  sync_cadence_ms: 100
  report_interval_ms: 1000
addressing:
  mode: sector_range
  start_sector: 8
  sector_count: 1024
  start_offset_bytes:
  range_size_bytes:
durability:
  mode: batch_durable
  fdatasync_at_end: true
verification:
  enabled: true
telemetry:
  health_telemetry: true
diagnostics:
  capture_during_test: true
  deep_trace: false
  sample_interval_ms: 1000
reporting:
  export_json: true
  export_csv: true
  export_html: true
  notes: []
safety:
  destructive_confirmation_required: true
  confirmation_flag: false
"#;
        let profile: Profile = serde_yaml::from_str(yaml).expect("yaml");
        profile.validate().expect("valid");
        assert_eq!(profile.workload.exact_op_count, Some(100));
        assert_eq!(profile.addressing.mode, AddressingMode::SectorRange);
    }

    #[test]
    fn validates_sector_range_alignment() {
        let mut profile = Profile::default_named("range");
        profile.addressing.mode = AddressingMode::SectorRange;
        profile.addressing.start_sector = Some(8);
        profile.addressing.sector_count = Some(64);
        let range = profile.resolved_range(1024 * 1024, 512).expect("range");
        assert_eq!(range.start_bytes, 4096);
        assert_eq!(range.length_bytes, 64 * 512);
    }

    #[test]
    fn rejects_missing_stop_condition() {
        let mut profile = Profile::default_named("invalid");
        profile.workload.runtime_seconds = None;
        profile.workload.exact_op_count = None;
        assert!(profile.validate().is_err());
    }
}
