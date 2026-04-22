use crate::profile::{AddressingMode, DurabilityMode, Profile, TargetMode, WorkloadType};
use crate::system::{AppPaths, DeviceInfo};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresetCategory {
    QuickFile,
    Partition,
    RawDevice,
    Focused,
    Diagnostics,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresetTargetScope {
    File,
    Partition,
    RawDevice,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresetId {
    QuickFileRandRead,
    QuickFileRandWrite,
    QuickFileRandRw,
    PartitionSeqRead,
    PartitionSeqWrite,
    PartitionRandRead,
    PartitionRandWrite,
    RawDiskSeqRead,
    RawDiskSeqWrite,
    OneSectorMillionReads,
    OneSectorMillionWrites,
    InterferenceCheck,
    ConcurrentPartitionStress,
}

#[derive(Debug, Clone, Copy)]
pub struct PresetSpec {
    pub id: PresetId,
    pub category: PresetCategory,
    pub target_scope: PresetTargetScope,
    pub name: &'static str,
    pub description: &'static str,
    pub destructive: bool,
}

pub const BUILTIN_PRESETS: &[PresetSpec] = &[
    PresetSpec {
        id: PresetId::QuickFileRandRead,
        category: PresetCategory::QuickFile,
        target_scope: PresetTargetScope::File,
        name: "Quick File Random Read",
        description: "Safe file-backed random reads against a runtime-managed sample file.",
        destructive: false,
    },
    PresetSpec {
        id: PresetId::QuickFileRandWrite,
        category: PresetCategory::QuickFile,
        target_scope: PresetTargetScope::File,
        name: "Quick File Random Write",
        description: "Safe file-backed random writes for a quick write sanity pass.",
        destructive: false,
    },
    PresetSpec {
        id: PresetId::QuickFileRandRw,
        category: PresetCategory::QuickFile,
        target_scope: PresetTargetScope::File,
        name: "Quick File Mixed Read/Write",
        description: "Safe mixed random read/write run with lightweight diagnostics disabled.",
        destructive: false,
    },
    PresetSpec {
        id: PresetId::PartitionSeqRead,
        category: PresetCategory::Partition,
        target_scope: PresetTargetScope::Partition,
        name: "Whole Partition Sequential Read",
        description: "Sequential read sweep across the selected partition.",
        destructive: false,
    },
    PresetSpec {
        id: PresetId::PartitionSeqWrite,
        category: PresetCategory::Partition,
        target_scope: PresetTargetScope::Partition,
        name: "Whole Partition Sequential Write",
        description: "Destructive sequential write sweep across the selected partition.",
        destructive: true,
    },
    PresetSpec {
        id: PresetId::PartitionRandRead,
        category: PresetCategory::Partition,
        target_scope: PresetTargetScope::Partition,
        name: "Whole Partition Random Read",
        description: "Random read characterization across the selected partition.",
        destructive: false,
    },
    PresetSpec {
        id: PresetId::PartitionRandWrite,
        category: PresetCategory::Partition,
        target_scope: PresetTargetScope::Partition,
        name: "Whole Partition Random Write",
        description: "Destructive random write characterization across the selected partition.",
        destructive: true,
    },
    PresetSpec {
        id: PresetId::RawDiskSeqRead,
        category: PresetCategory::RawDevice,
        target_scope: PresetTargetScope::RawDevice,
        name: "Whole Disk Sequential Read",
        description: "Sequential read sweep across the selected raw block device.",
        destructive: false,
    },
    PresetSpec {
        id: PresetId::RawDiskSeqWrite,
        category: PresetCategory::RawDevice,
        target_scope: PresetTargetScope::RawDevice,
        name: "Whole Disk Sequential Write",
        description: "Destructive sequential write sweep across the selected raw block device.",
        destructive: true,
    },
    PresetSpec {
        id: PresetId::OneSectorMillionReads,
        category: PresetCategory::Focused,
        target_scope: PresetTargetScope::RawDevice,
        name: "One Sector 1M Reads",
        description: "Repeat exactly 1,000,000 logical reads against one selected logical sector.",
        destructive: false,
    },
    PresetSpec {
        id: PresetId::OneSectorMillionWrites,
        category: PresetCategory::Focused,
        target_scope: PresetTargetScope::RawDevice,
        name: "One Sector 1M Writes",
        description: "Repeat exactly 1,000,000 logical writes against one selected logical sector.",
        destructive: true,
    },
    PresetSpec {
        id: PresetId::InterferenceCheck,
        category: PresetCategory::Diagnostics,
        target_scope: PresetTargetScope::File,
        name: "Interference Check",
        description: "File-backed mixed workload with diagnostic capture enabled to spot outside I/O interference.",
        destructive: false,
    },
    PresetSpec {
        id: PresetId::ConcurrentPartitionStress,
        category: PresetCategory::Diagnostics,
        target_scope: PresetTargetScope::Partition,
        name: "Concurrent Partition Stress",
        description: "Read selected boot-partition sectors while writing a user partition, with before/after md5 verification.",
        destructive: true,
    },
];

pub fn preset_categories() -> &'static [PresetCategory] {
    &[
        PresetCategory::QuickFile,
        PresetCategory::Partition,
        PresetCategory::RawDevice,
        PresetCategory::Focused,
        PresetCategory::Diagnostics,
    ]
}

pub fn category_label(category: PresetCategory) -> &'static str {
    match category {
        PresetCategory::QuickFile => "Quick File Presets",
        PresetCategory::Partition => "Whole Partition Presets",
        PresetCategory::RawDevice => "Whole Disk Raw Presets",
        PresetCategory::Focused => "Focused Logical Range Presets",
        PresetCategory::Diagnostics => "Diagnostics / Interference Presets",
    }
}

pub fn presets_in_category(category: PresetCategory) -> Vec<&'static PresetSpec> {
    BUILTIN_PRESETS
        .iter()
        .filter(|preset| preset.category == category)
        .collect()
}

pub fn resolve_preset(
    preset: &PresetSpec,
    paths: &AppPaths,
    device: Option<&DeviceInfo>,
    sector: Option<u64>,
) -> Profile {
    let mut profile = Profile::default_named(preset_slug(preset.id));
    profile.name = preset_slug(preset.id).to_string();
    profile.description = Some(preset.description.to_string());
    profile.reporting.notes.push(preset.description.to_string());
    profile.safety.confirmation_flag = false;

    match preset.target_scope {
        PresetTargetScope::File => {
            profile.target.mode = TargetMode::FileBased;
            profile.target.path = default_sample_path(paths, preset.id);
            profile.target.create_if_missing_size_bytes = Some(default_file_size(preset.id));
            profile.telemetry.health_telemetry = false;
        }
        PresetTargetScope::Partition | PresetTargetScope::RawDevice => {
            let target = device.expect("device required for block preset");
            profile.target.mode = TargetMode::RawDevice;
            profile.target.path = target.path.clone();
            profile.target.create_if_missing_size_bytes = None;
            profile.telemetry.health_telemetry = target.path.to_string_lossy().contains("mmcblk");
        }
    }

    apply_preset_shape(&mut profile, preset, device, sector);
    profile
}

fn apply_preset_shape(
    profile: &mut Profile,
    preset: &PresetSpec,
    device: Option<&DeviceInfo>,
    sector: Option<u64>,
) {
    profile.workload.queue_depth = 1;
    profile.workload.worker_count = 1;
    profile.workload.random_seed = 1;
    profile.workload.report_interval_ms = 1000;
    profile.diagnostics.capture_during_test = false;
    profile.diagnostics.deep_trace = false;
    profile.diagnostics.sample_interval_ms = 1000;
    profile.verification.enabled = false;
    profile.workload.verify = false;
    profile.safety.destructive_confirmation_required = true;

    match preset.id {
        PresetId::QuickFileRandRead => {
            profile.workload.test_type = WorkloadType::RandRead;
            profile.workload.runtime_seconds = Some(30);
            profile.workload.exact_op_count = None;
            profile.workload.block_size_bytes = 4096;
            profile.addressing.mode = AddressingMode::WholeSelectedRange;
            profile.workload.direct_io = false;
        }
        PresetId::QuickFileRandWrite => {
            profile.workload.test_type = WorkloadType::RandWrite;
            profile.workload.runtime_seconds = Some(30);
            profile.workload.exact_op_count = None;
            profile.workload.block_size_bytes = 4096;
            profile.addressing.mode = AddressingMode::WholeSelectedRange;
            profile.workload.direct_io = false;
            profile.durability.mode = DurabilityMode::BatchDurable;
        }
        PresetId::QuickFileRandRw => {
            profile.workload.test_type = WorkloadType::RandRw;
            profile.workload.runtime_seconds = Some(60);
            profile.workload.exact_op_count = None;
            profile.workload.block_size_bytes = 4096;
            profile.workload.read_percentage = 60;
            profile.addressing.mode = AddressingMode::WholeSelectedRange;
            profile.workload.direct_io = false;
            profile.durability.mode = DurabilityMode::BatchDurable;
        }
        PresetId::PartitionSeqRead => {
            profile.workload.test_type = WorkloadType::Read;
            profile.workload.runtime_seconds = Some(120);
            profile.workload.exact_op_count = None;
            profile.workload.block_size_bytes = device_sector(device);
            profile.addressing.mode = AddressingMode::WholeSelectedRange;
            profile.workload.direct_io = true;
        }
        PresetId::PartitionSeqWrite => {
            profile.workload.test_type = WorkloadType::Write;
            profile.workload.runtime_seconds = Some(120);
            profile.workload.exact_op_count = None;
            profile.workload.block_size_bytes = device_sector(device);
            profile.addressing.mode = AddressingMode::WholeSelectedRange;
            profile.workload.direct_io = true;
            profile.verification.enabled = true;
            profile.workload.verify = true;
            profile.durability.mode = DurabilityMode::BatchDurable;
        }
        PresetId::PartitionRandRead => {
            profile.workload.test_type = WorkloadType::RandRead;
            profile.workload.runtime_seconds = Some(120);
            profile.workload.exact_op_count = None;
            profile.workload.block_size_bytes = 4096.max(device_sector(device));
            profile.addressing.mode = AddressingMode::WholeSelectedRange;
            profile.workload.direct_io = true;
        }
        PresetId::PartitionRandWrite => {
            profile.workload.test_type = WorkloadType::RandWrite;
            profile.workload.runtime_seconds = Some(120);
            profile.workload.exact_op_count = None;
            profile.workload.block_size_bytes = 4096.max(device_sector(device));
            profile.addressing.mode = AddressingMode::WholeSelectedRange;
            profile.workload.direct_io = true;
            profile.verification.enabled = true;
            profile.workload.verify = true;
            profile.durability.mode = DurabilityMode::BatchDurable;
        }
        PresetId::RawDiskSeqRead => {
            profile.workload.test_type = WorkloadType::Read;
            profile.workload.runtime_seconds = Some(180);
            profile.workload.exact_op_count = None;
            profile.workload.block_size_bytes = device_sector(device);
            profile.addressing.mode = AddressingMode::WholeSelectedRange;
            profile.workload.direct_io = true;
        }
        PresetId::RawDiskSeqWrite => {
            profile.workload.test_type = WorkloadType::Write;
            profile.workload.runtime_seconds = Some(180);
            profile.workload.exact_op_count = None;
            profile.workload.block_size_bytes = device_sector(device);
            profile.addressing.mode = AddressingMode::WholeSelectedRange;
            profile.workload.direct_io = true;
            profile.verification.enabled = true;
            profile.workload.verify = true;
            profile.durability.mode = DurabilityMode::BatchDurable;
        }
        PresetId::OneSectorMillionReads => {
            let logical_sector = device_sector(device) as u64;
            profile.workload.test_type = WorkloadType::RandRead;
            profile.workload.runtime_seconds = None;
            profile.workload.exact_op_count = Some(1_000_000);
            profile.workload.block_size_bytes = logical_sector as usize;
            profile.addressing.mode = AddressingMode::SectorRange;
            profile.addressing.start_sector = Some(sector.unwrap_or(0));
            profile.addressing.sector_count = Some(1);
            profile.addressing.start_offset_bytes = None;
            profile.addressing.range_size_bytes = None;
            profile.workload.direct_io = true;
        }
        PresetId::OneSectorMillionWrites => {
            let logical_sector = device_sector(device) as u64;
            profile.workload.test_type = WorkloadType::RandWrite;
            profile.workload.runtime_seconds = None;
            profile.workload.exact_op_count = Some(1_000_000);
            profile.workload.block_size_bytes = logical_sector as usize;
            profile.addressing.mode = AddressingMode::SectorRange;
            profile.addressing.start_sector = Some(sector.unwrap_or(0));
            profile.addressing.sector_count = Some(1);
            profile.addressing.start_offset_bytes = None;
            profile.addressing.range_size_bytes = None;
            profile.workload.direct_io = true;
            profile.verification.enabled = true;
            profile.workload.verify = true;
            profile.durability.mode = DurabilityMode::BatchDurable;
        }
        PresetId::InterferenceCheck => {
            profile.workload.test_type = WorkloadType::RandRw;
            profile.workload.runtime_seconds = Some(90);
            profile.workload.exact_op_count = None;
            profile.workload.block_size_bytes = 4096;
            profile.workload.read_percentage = 60;
            profile.addressing.mode = AddressingMode::WholeSelectedRange;
            profile.workload.direct_io = false;
            profile.diagnostics.capture_during_test = true;
            profile.durability.mode = DurabilityMode::BatchDurable;
        }
        PresetId::ConcurrentPartitionStress => {
            profile.workload.test_type = WorkloadType::Write;
            profile.workload.runtime_seconds = None;
            profile.workload.exact_op_count = Some(1000);
            profile.workload.block_size_bytes = 200 * 1024;
            profile.addressing.mode = AddressingMode::ByteRange;
            profile.addressing.start_offset_bytes = Some(0);
            profile.addressing.range_size_bytes = Some(1024 * 1024 * 1024);
            profile.workload.direct_io = true;
            profile.durability.mode = DurabilityMode::BatchDurable;
            profile.telemetry.health_telemetry = true;
        }
    }
}

fn default_sample_path(paths: &AppPaths, preset: PresetId) -> PathBuf {
    paths
        .samples_dir
        .join(format!("{}.bin", preset_slug(preset)))
}

fn default_file_size(preset: PresetId) -> u64 {
    match preset {
        PresetId::InterferenceCheck => 512 * 1024 * 1024,
        _ => 1024 * 1024 * 1024,
    }
}

fn device_sector(device: Option<&DeviceInfo>) -> usize {
    device
        .and_then(|value| value.logical_block_size)
        .unwrap_or(512)
        .max(512) as usize
}

pub fn preset_slug(id: PresetId) -> &'static str {
    match id {
        PresetId::QuickFileRandRead => "quick-file-randread",
        PresetId::QuickFileRandWrite => "quick-file-randwrite",
        PresetId::QuickFileRandRw => "quick-file-randrw",
        PresetId::PartitionSeqRead => "partition-seq-read",
        PresetId::PartitionSeqWrite => "partition-seq-write",
        PresetId::PartitionRandRead => "partition-randread",
        PresetId::PartitionRandWrite => "partition-randwrite",
        PresetId::RawDiskSeqRead => "raw-disk-seq-read",
        PresetId::RawDiskSeqWrite => "raw-disk-seq-write",
        PresetId::OneSectorMillionReads => "one-sector-1m-reads",
        PresetId::OneSectorMillionWrites => "one-sector-1m-writes",
        PresetId::InterferenceCheck => "interference-check",
        PresetId::ConcurrentPartitionStress => "concurrent-partition-stress",
    }
}

#[cfg(test)]
mod tests {
    use super::{resolve_preset, PresetId, PresetTargetScope, BUILTIN_PRESETS};
    use crate::system::{AppPaths, DeviceInfo};
    use std::path::PathBuf;

    #[test]
    fn one_sector_preset_resolves_exact_count() {
        let base = tempfile::tempdir().expect("tempdir");
        let paths = AppPaths {
            base_dir: base.path().to_path_buf(),
            profiles_dir: base.path().join("profiles"),
            sessions_dir: base.path().join("sessions"),
            exports_dir: base.path().join("exports"),
            samples_dir: base.path().join("samples"),
        };
        let device = DeviceInfo {
            path: PathBuf::from("/dev/mmcblk0"),
            logical_block_size: Some(512),
            ..DeviceInfo::default()
        };
        let preset = BUILTIN_PRESETS
            .iter()
            .find(|preset| preset.id == PresetId::OneSectorMillionReads)
            .expect("preset");
        let profile = resolve_preset(preset, &paths, Some(&device), Some(2048));
        assert_eq!(profile.workload.exact_op_count, Some(1_000_000));
        assert_eq!(profile.addressing.start_sector, Some(2048));
        assert_eq!(profile.addressing.sector_count, Some(1));
        assert_eq!(profile.workload.block_size_bytes, 512);
    }

    #[test]
    fn file_preset_uses_samples_dir() {
        let base = tempfile::tempdir().expect("tempdir");
        let paths = AppPaths {
            base_dir: base.path().to_path_buf(),
            profiles_dir: base.path().join("profiles"),
            sessions_dir: base.path().join("sessions"),
            exports_dir: base.path().join("exports"),
            samples_dir: base.path().join("samples"),
        };
        let preset = BUILTIN_PRESETS
            .iter()
            .find(|preset| preset.target_scope == PresetTargetScope::File)
            .expect("file preset");
        let profile = resolve_preset(preset, &paths, None, None);
        assert!(profile.target.path.starts_with(&paths.samples_dir));
    }
}
