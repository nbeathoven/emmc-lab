use anyhow::Result;
use emmc_lab::app::{run_diag_trace, run_profile_session};
use emmc_lab::engine::execute_profile;
use emmc_lab::health::read_emmc_health;
use emmc_lab::integrity::ChecksumAlgorithm;
use emmc_lab::profile::{AddressingMode, Profile, TargetMode, WorkloadType};
use emmc_lab::report::{export_session, ExportFormat};
use emmc_lab::storage::{
    delete_session, load_session, load_session_with_intervals, save_session, session_dir,
    SessionRecord,
};
use emmc_lab::system::{assess_raw_target_safety, list_devices, AppPaths};
use std::fs;
use tempfile::TempDir;

#[cfg(target_os = "linux")]
use emmc_lab::filemap::{collect_file_map, filemap_report_to_csv};
#[cfg(target_os = "linux")]
use emmc_lab::lba_trace::blktrace_debugfs_available;

fn temp_paths() -> Result<(TempDir, AppPaths)> {
    let dir = tempfile::tempdir()?;
    let base = dir.path().join("data");
    let profiles = dir.path().join("profiles");
    let sessions = base.join("sessions");
    let exports = base.join("exports");
    let samples = base.join("samples");
    fs::create_dir_all(&profiles)?;
    fs::create_dir_all(&sessions)?;
    fs::create_dir_all(&exports)?;
    fs::create_dir_all(&samples)?;
    Ok((
        dir,
        AppPaths {
            base_dir: base,
            profiles_dir: profiles,
            sessions_dir: sessions,
            exports_dir: exports,
            samples_dir: samples,
        },
    ))
}

fn file_profile(path: &std::path::Path) -> Profile {
    let mut profile = Profile::default_named("integration");
    profile.target.mode = TargetMode::FileBased;
    profile.target.path = path.to_path_buf();
    profile.workload.test_type = WorkloadType::RandWrite;
    profile.workload.block_size_bytes = 4096;
    profile.workload.runtime_seconds = None;
    profile.workload.exact_op_count = Some(16);
    profile.workload.worker_count = 2;
    profile.addressing.mode = AddressingMode::ByteRange;
    profile.addressing.start_offset_bytes = Some(0);
    profile.addressing.range_size_bytes = Some(4096 * 32);
    profile.telemetry.health_telemetry = false;
    profile.reporting.export_json = false;
    profile.reporting.export_csv = false;
    profile.reporting.export_html = false;
    profile
}

#[test]
fn file_mode_exact_operation_stop_condition() -> Result<()> {
    let tmp = tempfile::tempdir()?;
    let target = tmp.path().join("workload.bin");
    let profile = file_profile(&target);
    let (summary, _intervals) = execute_profile(&profile)?;
    assert_eq!(summary.write_ios_completed, 16);
    assert_eq!(summary.read_ios_completed, 0);
    Ok(())
}

#[test]
fn missing_file_read_target_can_be_created_at_runtime() -> Result<()> {
    let tmp = tempfile::tempdir()?;
    let target = tmp.path().join("runtime-read.bin");
    let mut profile = Profile::default_named("runtime-read");
    profile.target.mode = TargetMode::FileBased;
    profile.target.path = target.clone();
    profile.target.create_if_missing_size_bytes = Some(4096 * 32);
    profile.workload.test_type = WorkloadType::RandRead;
    profile.workload.block_size_bytes = 4096;
    profile.workload.runtime_seconds = None;
    profile.workload.exact_op_count = Some(8);
    profile.addressing.mode = AddressingMode::ByteRange;
    profile.addressing.start_offset_bytes = Some(0);
    profile.addressing.range_size_bytes = Some(4096 * 8);
    profile.telemetry.health_telemetry = false;
    profile.reporting.export_json = false;
    profile.reporting.export_csv = false;
    profile.reporting.export_html = false;

    let (summary, _intervals) = execute_profile(&profile)?;
    assert_eq!(summary.read_ios_completed, 8);
    assert!(target.exists());
    assert_eq!(fs::metadata(&target)?.len(), 4096 * 32);
    Ok(())
}

#[test]
fn combined_test_and_diagnostics_session_is_saved() -> Result<()> {
    let (_dir, paths) = temp_paths()?;
    let target = paths.base_dir.join("combined.bin");
    let mut profile = file_profile(&target);
    profile.workload.runtime_seconds = Some(1);
    profile.workload.exact_op_count = None;
    profile.diagnostics.capture_during_test = true;
    let session_id = run_profile_session(&paths, profile)?;
    let session = load_session(&paths, &session_id)?;
    assert!(session.run_summary.is_some());
    assert!(session.diagnostics.is_some());
    Ok(())
}

#[test]
fn run_session_auto_verifies_integrity_regions() -> Result<()> {
    let (_dir, paths) = temp_paths()?;
    let target = paths.base_dir.join("integrity-target.bin");
    fs::write(&target, vec![0x5a; 4096 * 8])?;
    let mut profile = Profile::default_named("integrity-read");
    profile.target.mode = TargetMode::FileBased;
    profile.target.path = target.clone();
    profile.target.create_if_missing_size_bytes = None;
    profile.workload.test_type = WorkloadType::Read;
    profile.workload.block_size_bytes = 4096;
    profile.workload.runtime_seconds = None;
    profile.workload.exact_op_count = Some(4);
    profile.addressing.mode = AddressingMode::ByteRange;
    profile.addressing.start_offset_bytes = Some(0);
    profile.addressing.range_size_bytes = Some(4096 * 4);
    profile.telemetry.health_telemetry = false;
    profile.reporting.export_json = false;
    profile.reporting.export_csv = false;
    profile.reporting.export_html = false;
    profile
        .integrity
        .push(emmc_lab::profile::IntegrityRegionConfig {
            device: target,
            offset_bytes: 0,
            length_bytes: 4096 * 4,
            algorithm: ChecksumAlgorithm::Md5,
            capture_before_run: true,
        });

    let session_id = run_profile_session(&paths, profile)?;
    let session = load_session(&paths, &session_id)?;
    assert_eq!(session.integrity_baselines.len(), 1);
    assert_eq!(session.integrity_verifications.len(), 1);
    assert!(session.integrity_verifications[0].matches);
    assert!(session.integrity_baselines[0]
        .snapshot_path
        .as_ref()
        .is_some_and(|path| path.exists()));
    Ok(())
}

#[test]
fn session_json_is_summary_only_but_intervals_can_be_loaded() -> Result<()> {
    let (_dir, paths) = temp_paths()?;
    let target = paths.base_dir.join("summary-only.bin");
    let mut profile = file_profile(&target);
    profile.workload.runtime_seconds = Some(1);
    profile.workload.exact_op_count = None;
    let session_id = run_profile_session(&paths, profile)?;
    let summary = load_session(&paths, &session_id)?;
    let full = load_session_with_intervals(&paths, &session_id)?;
    assert!(summary.interval_stats.is_empty());
    assert!(!full.interval_stats.is_empty());
    Ok(())
}

#[test]
fn health_unavailable_fallback_is_nonfatal() -> Result<()> {
    let snapshot = read_emmc_health(std::path::Path::new("/dev/mmcblk0"), None, None)?;
    if !snapshot.available {
        assert!(snapshot.note.contains("unavailable"));
    }
    Ok(())
}

#[test]
fn deep_trace_unavailable_fallback_produces_session() -> Result<()> {
    let (_dir, paths) = temp_paths()?;
    let session_id = run_diag_trace(&paths, 1, true)?;
    let session = load_session(&paths, &session_id)?;
    assert!(session.diagnostics.is_some());
    Ok(())
}

#[test]
fn export_generation_writes_json_csv_html() -> Result<()> {
    let (_dir, paths) = temp_paths()?;
    let mut session = SessionRecord::new("export-test".to_string());
    session.notes.push("example".to_string());
    save_session(&paths, &session)?;
    let json = export_session(&paths, "export-test", ExportFormat::Json)?;
    let csv = export_session(&paths, "export-test", ExportFormat::Csv)?;
    let html = export_session(&paths, "export-test", ExportFormat::Html)?;
    assert!(!json.is_empty());
    assert!(!csv.is_empty());
    assert!(!html.is_empty());
    Ok(())
}

#[test]
fn delete_session_removes_saved_directory() -> Result<()> {
    let (_dir, paths) = temp_paths()?;
    let mut session = SessionRecord::new("delete-me".to_string());
    session.notes.push("example".to_string());
    save_session(&paths, &session)?;
    let dir = session_dir(&paths, "delete-me");
    assert!(dir.exists());
    delete_session(&paths, "delete-me")?;
    assert!(!dir.exists());
    Ok(())
}

#[test]
fn raw_mode_safety_refuses_mounted_or_nonblock_targets() -> Result<()> {
    if let Some(device) = list_devices()?.into_iter().find(|d| d.mounted) {
        let assessment = assess_raw_target_safety(&device.path, true, false)?;
        assert!(!assessment.allowed);
        return Ok(());
    }
    let assessment = assess_raw_target_safety(std::path::Path::new("/dev/null"), true, false)?;
    assert!(!assessment.allowed);
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn file_map_collects_extents_for_regular_file() -> Result<()> {
    let tmp = tempfile::tempdir()?;
    let target = tmp.path().join("mapped.bin");
    fs::write(&target, vec![0x5a; 64 * 1024])?;
    let report = collect_file_map(&target, None)?;
    assert!(!report.extents.is_empty());
    assert!(report.extent_count >= 1);
    assert_eq!(
        report.total_extent_bytes,
        report.extents.iter().map(|e| e.length).sum::<u64>()
    );
    assert!(matches!(report.method.as_str(), "fiemap" | "fibmap"));
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn file_map_csv_contains_expected_columns() -> Result<()> {
    let tmp = tempfile::tempdir()?;
    let target = tmp.path().join("mapped.csv.bin");
    fs::write(&target, vec![0x33; 16 * 1024])?;
    let report = collect_file_map(&target, None)?;
    let csv = filemap_report_to_csv(&report);
    assert!(csv.starts_with("logical_offset,physical_lba,length,flags,flag_names,erase_aligned"));
    assert!(csv.lines().count() >= 2);
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn blktrace_probe_returns_structured_result() -> Result<()> {
    let device = list_devices()?
        .into_iter()
        .find(|dev| dev.name.starts_with("mmcblk"))
        .map(|dev| dev.path)
        .unwrap_or_else(|| std::path::PathBuf::from("/dev/mmcblk0"));
    let check = blktrace_debugfs_available(&device.display().to_string())?;
    assert_eq!(check.name, "blktrace_debugfs");
    assert!(!check.detail.trim().is_empty());
    Ok(())
}
