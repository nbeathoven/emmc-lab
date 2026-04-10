use anyhow::Result;
use emmc_lab::app::{run_diag_trace, run_profile_session};
use emmc_lab::engine::execute_profile;
use emmc_lab::health::read_emmc_health;
use emmc_lab::profile::{AddressingMode, Profile, TargetMode, WorkloadType};
use emmc_lab::report::{export_session, ExportFormat};
use emmc_lab::storage::{load_session, load_session_with_intervals, save_session, SessionRecord};
use emmc_lab::system::{assess_raw_target_safety, list_devices, AppPaths};
use std::fs;
use tempfile::TempDir;

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
    let snapshot = read_emmc_health(std::path::Path::new("/dev/mmcblk0"))?;
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
