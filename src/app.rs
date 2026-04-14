use crate::diagnostics::{
    run_deep_trace, run_live_sampler, run_live_sampler_until, DiagnosticReport, LiveSamplerState,
};
use crate::engine::execute_profile;
use crate::filemap;
use crate::geometry::collect_block_geometry;
use crate::health::{
    build_read_disturb_model, collect_health, format_raw_extcsd_dump, read_emmc_health,
    ReadDisturbInput,
};
use crate::health_compare::{self, render_health_compare_html};
use crate::lba_trace::{self, LbaTraceOptions};
use crate::presets::{
    category_label, preset_categories, presets_in_category, resolve_preset, PresetId,
    PresetTargetScope,
};
use crate::profile::{AddressingMode, DurabilityMode, Profile, TargetMode, WorkloadType};
use crate::report::{export_session, terminal_summary, ExportFormat};
use crate::storage::{
    delete_session, infer_profile_path, list_profiles, list_sessions, load_session, save_session,
    SessionRecord,
};
use crate::system::{
    assess_raw_target_safety, block_device_size, collect_capabilities, collect_system_snapshot,
    diskstats_map, doctor, list_devices, logical_block_size, AppPaths, DeviceInfo,
};
use crate::ui::{
    render_device_geometry, render_device_list, render_doctor_report, render_health_report,
    render_live_monitor, render_selector_screen, render_settings, SelectorColumn, SelectorRow,
};
use anyhow::{anyhow, bail, Result};
use chrono::{Local, Utc};
use std::env;
use std::ffi::CString;
use std::fs;
use std::io::{self, Write};
use std::mem::MaybeUninit;
use std::os::unix::ffi::OsStrExt;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::process;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct WizardOutcome {
    pub profile: Profile,
    pub run_now: bool,
}

#[derive(Debug, Clone)]
struct WizardTargetHints {
    target_label: String,
    size_bytes: Option<u64>,
    logical_sector_size: u64,
    health_supported: bool,
}

enum WizardInput<T> {
    Value(T),
    Back,
    Cancel,
}

struct MenuItem {
    columns: Vec<String>,
    detail: String,
}

struct SessionMenuEntry {
    session_id: String,
    columns: Vec<String>,
    detail: String,
}

struct Spinner {
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

enum MenuKey {
    Up,
    Down,
    Enter,
    Escape,
    Quit,
    Resize,
}

static RESIZE_FLAG: AtomicBool = AtomicBool::new(false);

extern "C" fn handle_sigwinch(_: libc::c_int) {
    RESIZE_FLAG.store(true, Ordering::Relaxed);
}

pub fn run_menu(paths: &AppPaths) -> Result<()> {
    let mut last_profile = None::<Profile>;
    loop {
        let items = vec![
            MenuItem {
                columns: vec!["Run Workload".to_string(), "presets, wizard, saved, repeat".to_string()],
                detail: "Scope: file targets or raw devices. Risk: varies by selected workflow and can become destructive on raw-device write runs. Open workload entry points including quick presets, guided custom runs, saved profiles, and repeat-last-run.".to_string(),
            },
            MenuItem {
                columns: vec!["Diagnostics".to_string(), "live monitor, capture, deep trace".to_string()],
                detail: "Scope: local diagnostics. Risk: read-only sampling unless you choose deeper tracing support. Launch the live monitor, save a timed capture, or try deep trace with fallback guidance.".to_string(),
            },
            MenuItem {
                columns: vec!["Device Health".to_string(), "mmc health and host snapshot".to_string()],
                detail: "Scope: selected block device. Risk: read-only. Inspect eMMC health and system context for a selected block device.".to_string(),
            },
            MenuItem {
                columns: vec!["Reports".to_string(), "view or export sessions".to_string()],
                detail: "Scope: saved session data. Risk: viewing is read-only, export writes local files, and delete removes saved data. Browse saved sessions and export JSON, CSV, or HTML reports.".to_string(),
            },
            MenuItem {
                columns: vec!["Settings".to_string(), "paths and capabilities".to_string()],
                detail: "Scope: local environment. Risk: read-only. Show application paths, system defaults, and capability checks.".to_string(),
            },
            MenuItem {
                columns: vec!["Help".to_string(), "commands and workflow notes".to_string()],
                detail: "Scope: operator guidance. Risk: read-only. Show built-in CLI usage examples and workflow notes.".to_string(),
            },
            MenuItem {
                columns: vec!["Exit".to_string(), "leave emmc-lab".to_string()],
                detail: "Scope: current console session. Risk: none. Return to the shell.".to_string(),
            },
        ];
        let Some(choice) = select_from_menu(
            "EMMC-LAB OPERATOR CONSOLE",
            "Main Menu",
            vec![
                (
                    "Host".to_string(),
                    collect_system_snapshot()
                        .hostname
                        .unwrap_or_else(|| "-".to_string()),
                ),
                (
                    "Profiles".to_string(),
                    paths.profiles_dir.display().to_string(),
                ),
                (
                    "Sessions".to_string(),
                    paths.sessions_dir.display().to_string(),
                ),
            ],
            &[
                SelectorColumn {
                    header: "Action",
                    min: 16,
                    max: 22,
                    align_right: false,
                },
                SelectorColumn {
                    header: "Purpose",
                    min: 24,
                    max: 42,
                    align_right: false,
                },
            ],
            &items,
            0,
        )?
        else {
            break;
        };
        match choice {
            0 => run_workload_flow(paths, &mut last_profile)?,
            1 => diagnostics_flow(paths)?,
            2 => {
                health_flow(paths)?;
                pause()?;
            }
            3 => reports_flow(paths)?,
            4 => {
                settings_flow(paths)?;
                pause()?;
            }
            5 => {
                print_help(paths);
                pause()?;
            }
            6 => break,
            _ => unreachable!(),
        }
    }
    Ok(())
}

fn run_workload_flow(paths: &AppPaths, last_profile: &mut Option<Profile>) -> Result<()> {
    loop {
        let items = vec![
            MenuItem {
                columns: vec!["Quick Presets".to_string(), "named common workloads".to_string()],
                detail: "Scope: workload templates. Risk: depends on the chosen preset; raw-device write presets are destructive. Choose from built-in file, partition, raw-device, focused logical-range, and diagnostics-oriented presets.".to_string(),
            },
            MenuItem {
                columns: vec!["Guided Custom Run".to_string(), "full wizard".to_string()],
                detail: "Scope: full workload definition. Risk: depends on your target mode and test type. Open the guided wizard and build a custom profile from scratch.".to_string(),
            },
            MenuItem {
                columns: vec!["Saved Profiles".to_string(), "reload existing YAML".to_string()],
                detail: "Scope: saved YAML profiles. Risk: running a selected profile may write to its target. Browse saved YAML profiles and run or edit them.".to_string(),
            },
            MenuItem {
                columns: vec!["Repeat Last Run".to_string(), "rerun current session profile".to_string()],
                detail: "Scope: most recent workload profile. Risk: identical to the previous run profile, including destructive raw-device writes if that was the last action. Run the most recent workload profile again without re-entering fields.".to_string(),
            },
            MenuItem {
                columns: vec!["Back".to_string(), "return to main menu".to_string()],
                detail: "Leave workload management and return to the main operator console.".to_string(),
            },
        ];
        let Some(choice) = select_from_menu(
            "EMMC-LAB WORKLOADS",
            "Run Workload",
            vec![(
                "Last Run".to_string(),
                last_profile
                    .as_ref()
                    .map(|profile| profile.name.clone())
                    .unwrap_or_else(|| "-".to_string()),
            )],
            &[
                SelectorColumn {
                    header: "Action",
                    min: 18,
                    max: 22,
                    align_right: false,
                },
                SelectorColumn {
                    header: "Purpose",
                    min: 22,
                    max: 42,
                    align_right: false,
                },
            ],
            &items,
            0,
        )?
        else {
            return Ok(());
        };
        match choice {
            0 => quick_presets_flow(paths, last_profile)?,
            1 => {
                if let Some(outcome) = wizard(paths, None)? {
                    if outcome.run_now {
                        let profile = outcome.profile.clone();
                        run_profile_session_interactive(paths, outcome.profile)?;
                        *last_profile = Some(profile);
                    }
                }
            }
            2 => run_saved_profile_flow(paths, last_profile)?,
            3 => {
                let Some(profile) = last_profile.clone() else {
                    println!("No previous workload run in this session.");
                    continue;
                };
                run_profile_session_interactive(paths, profile.clone())?;
                *last_profile = Some(profile);
            }
            4 => return Ok(()),
            _ => unreachable!(),
        }
    }
}

pub fn run_profile_session(paths: &AppPaths, profile: Profile) -> Result<String> {
    profile.validate()?;
    if matches!(profile.target.mode, TargetMode::RawDevice) {
        let safety = assess_raw_target_safety(
            &profile.target.path,
            profile.is_write_workload(),
            profile.safety.confirmation_flag,
        )?;
        if !safety.allowed {
            bail!("raw target refused: {}", safety.reasons.join("; "));
        }
    }

    let session_id = session_id();
    let mut record = SessionRecord::new(session_id.clone());
    record.profile = Some(profile.clone());
    record.notes.push(
        "Logical sector/LBA targeting only. The application does not claim fixed physical NAND cell targeting."
            .to_string(),
    );

    if profile.telemetry.health_telemetry {
        let target_label = operator_target_label(&profile.target.path);
        record.health_before = Some(with_spinner(
            &format!("Collecting pre-run health for {target_label}"),
            || read_emmc_health(&profile.target.path, None, None),
        )?);
    }

    let stop_signal = Arc::new(AtomicBool::new(false));
    let diag_handle = if profile.diagnostics.capture_during_test {
        let interval = Duration::from_millis(profile.diagnostics.sample_interval_ms.max(250));
        let session = session_id.clone();
        let stop = Arc::clone(&stop_signal);
        Some(thread::spawn(move || {
            run_live_sampler_until(interval, Some(session), stop)
        }))
    } else {
        None
    };

    let target_label = operator_target_label(&profile.target.path);
    let (run_summary, intervals) =
        with_spinner(&format!("Running workload on {target_label}"), || {
            execute_profile(&profile)
        })?;
    record.run_summary = Some(run_summary);
    record.interval_stats = intervals;

    if profile.telemetry.health_telemetry {
        let target_label = operator_target_label(&profile.target.path);
        record.health_after = Some(with_spinner(
            &format!("Collecting post-run health for {target_label}"),
            || read_emmc_health(&profile.target.path, None, None),
        )?);
    }

    if let Some(handle) = diag_handle {
        stop_signal.store(true, Ordering::Relaxed);
        if let Ok(Ok(diag)) = handle.join() {
            record.contamination_note = derive_interference_note(&diag);
            record.diagnostics = Some(diag);
        }
    }

    let path = with_spinner("Saving session", || save_session(paths, &record))?;
    println!("Saved session: {}", path.display());

    if profile.reporting.export_json {
        let _ = with_spinner("Exporting JSON", || {
            export_session(paths, &record.session_id, ExportFormat::Json)
        });
    }
    if profile.reporting.export_csv {
        let _ = with_spinner("Exporting CSV", || {
            export_session(paths, &record.session_id, ExportFormat::Csv)
        });
    }
    if profile.reporting.export_html {
        let _ = with_spinner("Exporting HTML", || {
            export_session(paths, &record.session_id, ExportFormat::Html)
        });
    }

    println!("{}", terminal_summary(&record));
    Ok(session_id)
}

pub fn run_profile_path(
    paths: &AppPaths,
    profile_path: &Path,
    destructive_confirmation: bool,
) -> Result<String> {
    let mut profile = Profile::load(profile_path)?;
    profile.safety.confirmation_flag = destructive_confirmation;
    run_profile_session(paths, profile)
}

pub fn run_health(
    paths: &AppPaths,
    device: &Path,
    raw_extcsd: bool,
    page_size_kb: Option<u64>,
    pages_per_block: Option<u64>,
    read_disturb_threshold: u64,
) -> Result<()> {
    let target_label = operator_target_label(device);
    let report = with_spinner(&format!("Collecting health for {target_label}"), || {
        collect_health(paths, device, page_size_kb, pages_per_block)
    })?;
    println!("{}", render_health_report(&report));
    if let (Some(page_size_kb), Some(pages_per_block), Some(sec_count), Some(total_bytes_read)) = (
        page_size_kb,
        pages_per_block,
        report.emmc.sec_count,
        host_total_bytes_read(device),
    ) {
        let model = build_read_disturb_model(ReadDisturbInput {
            total_bytes_read,
            page_size_kb,
            pages_per_block,
            read_disturb_threshold,
            device_capacity_bytes: sec_count.saturating_mul(512),
        });
        println!("Read Disturb Estimate:");
        println!(
            "  estimated_reads_per_block={:.4}",
            model.estimated_reads_per_block
        );
        println!("  threshold_ratio={:.4}", model.threshold_ratio);
        println!("  risk_label={}", model.risk_label);
        for caveat in model.caveats {
            println!("  note: {caveat}");
        }
    }
    if raw_extcsd {
        if let Some(raw) = report
            .emmc
            .ext_csd_raw
            .as_deref()
            .and_then(|bytes| <&[u8; 512]>::try_from(bytes).ok())
        {
            println!("{}", format_raw_extcsd_dump(raw));
        } else if let Some(raw) = &report.emmc.raw_text {
            println!("{raw}");
        } else {
            println!("Raw EXT_CSD dump unavailable.");
        }
    }
    Ok(())
}

pub fn run_diag_sample(
    paths: &AppPaths,
    duration_seconds: u64,
    interval_ms: u64,
) -> Result<String> {
    let session_id = session_id();
    let mut record = SessionRecord::new(session_id.clone());
    let report = with_spinner("Capturing diagnostics", || {
        run_live_sampler(
            Duration::from_secs(duration_seconds.max(1)),
            Duration::from_millis(interval_ms.max(100)),
            Some(session_id.clone()),
        )
    })?;
    record.diagnostics = Some(report);
    record.notes.push(
        "Logical syscall bytes are reported separately from storage-layer bytes in procfs sampler mode."
            .to_string(),
    );
    let path = with_spinner("Saving session", || save_session(paths, &record))?;
    println!("Saved session: {}", path.display());
    println!("{}", terminal_summary(&record));
    Ok(session_id)
}

pub fn run_file_map(file: &Path, device: Option<&Path>, csv: bool) -> Result<()> {
    let report = filemap::collect_file_map(file, device)?;
    if csv {
        print!("{}", filemap::filemap_report_to_csv(&report));
        return Ok(());
    }

    println!("File: {}", report.file_path.display());
    println!("Device: {}", report.device.display());
    println!("Method: {}", report.method);
    println!("Sector Size: {}", report.sector_size);
    if let Some(start) = report.partition_start_lba {
        println!("Partition Start LBA: {start}");
    }
    if let Some(erase_block_size) = report.erase_block_size {
        println!("Erase Block Size: {erase_block_size}");
    }
    println!("Extents: {}", report.extent_count);
    println!(
        "{:<6} {:>14} {:>14} {:>12}  Flags",
        "Index", "Logical", "Physical LBA", "Length"
    );
    for (index, extent) in report.extents.iter().enumerate() {
        let physical_lba = extent.physical_offset / report.sector_size.max(1);
        let flags = if extent.flag_names.is_empty() {
            "-".to_string()
        } else {
            extent.flag_names.join("|")
        };
        println!(
            "{:<6} {:>14} {:>14} {:>12}  {}",
            index, extent.logical_offset, physical_lba, extent.length, flags
        );
    }
    Ok(())
}

pub fn run_lba_trace(
    device: &Path,
    duration: u64,
    lba_range: Option<&str>,
    map_files: &[PathBuf],
) -> Result<()> {
    let lba_filter = lba_range.map(parse_lba_range).transpose()?;
    let report = lba_trace::capture_lba_trace(LbaTraceOptions {
        device: device.display().to_string(),
        duration_secs: duration,
        lba_filter,
        map_files: map_files.to_vec(),
    })?;
    println!("Device: {}", report.device);
    println!("Method: {}", report.method);
    println!("Duration: {}s", report.duration_secs);
    println!("Events: {}", report.event_count);
    for event in &report.events {
        let summary =
            if let Some(hit) = lba_trace::match_lba_to_file(event.lba, &report.owner_index) {
                format!(" -> {} @ {}", hit.file.display(), hit.offset_in_file)
            } else {
                String::new()
            };
        println!(
            "ts={} dir={:?} lba={} sectors={}{}",
            event.timestamp_ns, event.direction, event.lba, event.length_sectors, summary
        );
    }
    Ok(())
}

pub fn run_health_compare(
    paths: &AppPaths,
    device: &Path,
    fa_report: &Path,
    trace_report: Option<&Path>,
    html_output: Option<&Path>,
) -> Result<()> {
    let trace_report = match trace_report {
        Some(path) => Some(
            serde_json::from_str::<lba_trace::LbaTraceReport>(
                &fs::read_to_string(path)
                    .map_err(|err| anyhow::anyhow!("failed to read {}: {err}", path.display()))?,
            )
            .map_err(|err| anyhow::anyhow!("failed to parse {}: {err}", path.display()))?,
        ),
        None => None,
    };
    let report =
        health_compare::compare_health(paths, device, fa_report, trace_report.as_ref(), None)?;
    println!(
        "{:<20} {:<12} {:<12} {:<10} Detail",
        "Field", "Local", "FA", "Status"
    );
    for note in &report.field_comparisons {
        println!(
            "{:<20} {:<12} {:<12} {:<10} {}",
            note.field, note.local_value, note.fa_value, note.status, note.detail
        );
    }
    if let Some(html_output) = html_output {
        fs::write(html_output, render_health_compare_html(&report))
            .map_err(|err| anyhow::anyhow!("failed to write {}: {err}", html_output.display()))?;
        println!("HTML report: {}", html_output.display());
    }
    Ok(())
}

pub fn run_diag_monitor(duration_seconds: u64, interval_ms: u64) -> Result<()> {
    if unsafe { libc::isatty(libc::STDIN_FILENO) } != 1
        || unsafe { libc::isatty(libc::STDOUT_FILENO) } != 1
    {
        bail!("live monitor requires an interactive terminal; use `emmc-lab diag sample` for a saved capture");
    }
    let interval = Duration::from_millis(interval_ms.max(250));
    let _terminal = RawTerminalGuard::new()?;
    let _resize = ResizeSignalGuard::install();
    let mut sampler = LiveSamplerState::new(None)?;
    let mut last_report = sampler.sample_once()?;
    redraw_live_monitor(&last_report, interval_ms)?;
    loop {
        if RESIZE_FLAG.swap(false, Ordering::Relaxed) {
            redraw_live_monitor(&last_report, interval_ms)?;
        }
        if let Some(key) = poll_keypress_until(interval)? {
            if matches!(key, b'q' | b'Q' | 3) {
                break;
            }
        }
        if RESIZE_FLAG.swap(false, Ordering::Relaxed) {
            redraw_live_monitor(&last_report, interval_ms)?;
            continue;
        }
        last_report = sampler.sample_once()?;
        redraw_live_monitor(&last_report, interval_ms)?;
        if duration_seconds > 0 && last_report.duration_seconds >= duration_seconds {
            break;
        }
    }
    Ok(())
}

pub fn run_diag_trace(
    paths: &AppPaths,
    duration_seconds: u64,
    fallback_to_sampler: bool,
) -> Result<String> {
    let session_id = session_id();
    let mut record = SessionRecord::new(session_id.clone());
    let report = with_spinner("Running trace", || {
        run_deep_trace(
            Duration::from_secs(duration_seconds.max(1)),
            Some(session_id.clone()),
            fallback_to_sampler,
        )
    })?;
    record.diagnostics = Some(report);
    let path = with_spinner("Saving session", || save_session(paths, &record))?;
    if let Some(message) = record
        .diagnostics
        .as_ref()
        .and_then(|diag| diag.fallback_message.as_deref())
    {
        eprintln!("{}", message);
    }
    if unsafe { libc::isatty(libc::STDOUT_FILENO) } == 1 {
        println!("Saved session: {}", path.display());
        println!("{}", terminal_summary(&record));
    } else {
        eprintln!("Saved session: {}", path.display());
    }
    Ok(session_id)
}

pub fn run_report(paths: &AppPaths, session_id: &str) -> Result<()> {
    let record = load_session(paths, session_id)?;
    println!("{}", terminal_summary(&record));
    Ok(())
}

pub fn run_export(paths: &AppPaths, session_id: &str, format: ExportFormat) -> Result<()> {
    let files = with_spinner("Exporting report", || {
        export_session(paths, session_id, format)
    })?;
    for file in files {
        println!("{}", file.display());
    }
    Ok(())
}

pub fn run_list_devices() -> Result<()> {
    println!(
        "{}",
        render_device_list(&preferred_devices(list_devices()?))
    );
    Ok(())
}

pub fn run_doctor(paths: &AppPaths) -> Result<()> {
    let report = with_spinner("Running doctor checks", || doctor(paths));
    println!("{}", render_doctor_report(&report));
    Ok(())
}

pub fn run_geometry(device: &Path) -> Result<()> {
    let geometry = collect_block_geometry(device)?
        .ok_or_else(|| anyhow!("no geometry data available for {}", device.display()))?;
    println!("{}", render_device_geometry(&geometry));
    Ok(())
}

pub fn wizard(paths: &AppPaths, seed_profile: Option<Profile>) -> Result<Option<WizardOutcome>> {
    let mut profile = seed_profile.unwrap_or_else(|| Profile::default_named("new-test"));
    let mut step = 0_i32;
    let stop_mode_exact = profile.workload.exact_op_count.is_some();
    if !stop_mode_exact && profile.workload.runtime_seconds.is_none() {
        profile.workload.runtime_seconds = Some(30);
    }
    if profile.name.trim().is_empty() {
        profile.name = default_profile_name(&profile);
    }
    println!("Tip: Enter keeps the default. Type ? for a short explanation. Type back or cancel to leave a step.");
    while step < 15 {
        let hints = wizard_target_hints(&profile);
        match step {
            0 => {
                match prompt_menu_default_help(
                    "Mode [1=raw, 2=file]",
                    target_mode_choice(&profile.target.mode),
                    &["1", "2"],
                    Some(
                        "raw uses a block device directly and can be destructive; file uses a regular file and is the safer default.",
                    ),
                )? {
                    WizardInput::Value(value) => {
                        profile.target.mode = if value == "1" {
                            TargetMode::RawDevice
                        } else {
                            TargetMode::FileBased
                        };
                        profile.target.path = default_target_path(&profile.target.mode);
                    }
                    WizardInput::Back => {}
                    WizardInput::Cancel => return Ok(None),
                }
            }
            1 => match prompt_target_path(&profile.target.mode)? {
                WizardInput::Value(path) => {
                    let normalized =
                        normalize_file_target_path(&profile.target.mode, PathBuf::from(path));
                    profile.target.path = normalized;
                    let hints = wizard_target_hints(&profile);
                    println!("Target: {}", hints.target_label);
                    if profile.target.mode == TargetMode::FileBased
                        && !profile.target.path.exists()
                        && !profile.target.path.is_dir()
                    {
                        let default_size =
                            suggested_runtime_file_size(&profile, &hints).max(1_048_576);
                        let min_size =
                            minimum_runtime_file_size(&profile, hints.logical_sector_size)
                                .max(1_048_576);
                        let max_size = maximum_runtime_file_size(&profile.target.path)
                            .map(|bytes| bytes.max(min_size))
                            .unwrap_or(default_size.saturating_mul(64).max(min_size));
                        match prompt_u64_in_range(
                            &format!(
                                "Create file if missing [runtime, rec {} bytes]",
                                format_bytes(default_size)
                            ),
                            default_size.clamp(min_size, max_size),
                            min_size,
                            max_size,
                            Some(
                                "If the file does not exist, create it at run time with this size. Existing files are left alone.",
                            ),
                        )? {
                            WizardInput::Value(value) => {
                                profile.target.create_if_missing_size_bytes = Some(value);
                            }
                            WizardInput::Back => continue,
                            WizardInput::Cancel => return Ok(None),
                        }
                    } else {
                        profile.target.create_if_missing_size_bytes = None;
                    }
                }
                WizardInput::Back => {
                    step -= 1;
                    continue;
                }
                WizardInput::Cancel => return Ok(None),
            },
            2 => {
                match prompt_menu_default_help(
                    "Workload [1=randread, 2=randwrite, 3=randrw, 4=read, 5=write]",
                    workload_choice(&profile.workload.test_type),
                    &["1", "2", "3", "4", "5"],
                    Some(
                        "randread and randwrite pick random offsets, read and write scan sequentially, and randrw mixes both.",
                    ),
                )? {
                    WizardInput::Value(value) => {
                        profile.workload.test_type = match value.as_str() {
                            "1" => WorkloadType::RandRead,
                            "2" => WorkloadType::RandWrite,
                            "3" => WorkloadType::RandRw,
                            "4" => WorkloadType::Read,
                            "5" => WorkloadType::Write,
                            _ => unreachable!(),
                        };
                    }
                    WizardInput::Back => {
                        step -= 1;
                        continue;
                    }
                    WizardInput::Cancel => return Ok(None),
                }
            }
            3 => match prompt_menu_default_help(
                &format!(
                    "Range [1=all, 2=sectors, 3=bytes, 4=one sector] [{}]",
                    range_hint_label(&hints)
                ),
                addressing_choice(&profile.addressing.mode),
                &["1", "2", "3", "4"],
                Some(
                    "all uses the whole target, sectors uses logical LBAs, bytes uses byte offsets, and one sector repeats against a single logical sector.",
                ),
            )? {
                WizardInput::Value(value) => match value.as_str() {
                    "1" => {
                        profile.addressing.mode = AddressingMode::WholeSelectedRange;
                        profile.addressing.start_sector = None;
                        profile.addressing.sector_count = None;
                        profile.addressing.start_offset_bytes = Some(0);
                        profile.addressing.range_size_bytes = None;
                    }
                    "2" => {
                        let max_sector = hints
                            .size_bytes
                            .unwrap_or(1024 * 1024 * 1024)
                            .saturating_div(hints.logical_sector_size.max(1))
                            .saturating_sub(1);
                        let start_sector = match prompt_u64_in_range(
                            "Start sector",
                            profile.addressing.start_sector.unwrap_or(0),
                            0,
                            max_sector,
                            Some("Logical sector number from the start of the target. This is LBA addressing, not a physical NAND location."),
                        )? {
                            WizardInput::Value(value) => value,
                            WizardInput::Back => continue,
                            WizardInput::Cancel => return Ok(None),
                        };
                        let max_count = max_sector.saturating_sub(start_sector).saturating_add(1);
                        let default_count = profile
                            .addressing
                            .sector_count
                            .unwrap_or(max_count.min(1_048_576))
                            .min(max_count.max(1));
                        let sector_count = match prompt_u64_in_range(
                            "Sector count",
                            default_count.max(1),
                            1,
                            max_count.max(1),
                            Some("How many logical sectors to include starting at the chosen start sector."),
                        )? {
                            WizardInput::Value(value) => value,
                            WizardInput::Back => continue,
                            WizardInput::Cancel => return Ok(None),
                        };
                        profile.addressing.mode = AddressingMode::SectorRange;
                        profile.addressing.start_sector = Some(start_sector);
                        profile.addressing.sector_count = Some(sector_count);
                        profile.addressing.start_offset_bytes = None;
                        profile.addressing.range_size_bytes = None;
                    }
                    "3" => {
                        let max_offset = hints
                            .size_bytes
                            .unwrap_or(1024 * 1024 * 1024)
                            .saturating_sub(1);
                        let start_offset = match prompt_u64_in_range(
                            &format!(
                                "Start offset bytes [sector {} B]",
                                hints.logical_sector_size
                            ),
                            profile.addressing.start_offset_bytes.unwrap_or(0),
                            0,
                            max_offset,
                            Some("Byte offset from the start of the target."),
                        )? {
                            WizardInput::Value(value) => value,
                            WizardInput::Back => continue,
                            WizardInput::Cancel => return Ok(None),
                        };
                        let max_range = hints
                            .size_bytes
                            .unwrap_or(1024 * 1024 * 1024)
                            .saturating_sub(start_offset);
                        let default_range = profile
                            .addressing
                            .range_size_bytes
                            .unwrap_or(max_range.min(128 * 1024 * 1024))
                            .min(max_range.max(1));
                        let range_bytes = match prompt_u64_in_range(
                            "Range bytes",
                            default_range.max(1),
                            1,
                            max_range.max(1),
                            Some("How many bytes to include starting from the chosen byte offset."),
                        )? {
                            WizardInput::Value(value) => value,
                            WizardInput::Back => continue,
                            WizardInput::Cancel => return Ok(None),
                        };
                        profile.addressing.mode = AddressingMode::ByteRange;
                        profile.addressing.start_offset_bytes = Some(start_offset);
                        profile.addressing.range_size_bytes = Some(range_bytes);
                        profile.addressing.start_sector = None;
                        profile.addressing.sector_count = None;
                    }
                    "4" => {
                        let max_sector = hints
                            .size_bytes
                            .unwrap_or(1024 * 1024 * 1024)
                            .saturating_div(hints.logical_sector_size.max(1))
                            .saturating_sub(1);
                        let start_sector = match prompt_u64_in_range(
                            "Target sector [single logical sector]",
                            profile.addressing.start_sector.unwrap_or(0),
                            0,
                            max_sector,
                            Some("A single logical sector to hit repeatedly. This is useful for exact-count logical-sector tests."),
                        )? {
                            WizardInput::Value(value) => value,
                            WizardInput::Back => continue,
                            WizardInput::Cancel => return Ok(None),
                        };
                        profile.addressing.mode = AddressingMode::SectorRange;
                        profile.addressing.start_sector = Some(start_sector);
                        profile.addressing.sector_count = Some(1);
                        profile.addressing.start_offset_bytes = None;
                        profile.addressing.range_size_bytes = None;
                        profile.workload.block_size_bytes = hints.logical_sector_size as usize;
                        profile.workload.queue_depth = 1;
                        profile.workload.worker_count = 1;
                        if profile.workload.exact_op_count.is_none() {
                            profile.workload.exact_op_count = Some(1_000_000);
                            profile.workload.runtime_seconds = None;
                        }
                        println!(
                            "Pinned to sector {}. Defaults: block {} B, queue 1, workers 1, ops 1000000.",
                            start_sector, hints.logical_sector_size
                        );
                    }
                    _ => unreachable!(),
                },
                WizardInput::Back => {
                    step -= 1;
                    continue;
                }
                WizardInput::Cancel => return Ok(None),
            },
            4 => match prompt_menu_default_help(
                "Stop [1=time, 2=ops]",
                stop_choice(profile.workload.exact_op_count.is_some()),
                &["1", "2"],
                Some(
                    "time stops after a runtime in seconds; ops stops after an exact number of completed I/O operations.",
                ),
            )? {
                WizardInput::Value(value) => match value.as_str() {
                    "1" => {
                        let runtime = match prompt_u64_in_range(
                            "Runtime seconds",
                            profile.workload.runtime_seconds.unwrap_or(30),
                            5,
                            86_400,
                            Some("How long to run before stopping."),
                        )? {
                            WizardInput::Value(value) => value,
                            WizardInput::Back => continue,
                            WizardInput::Cancel => return Ok(None),
                        };
                        profile.workload.runtime_seconds = Some(runtime);
                        profile.workload.exact_op_count = None;
                    }
                    "2" => {
                        let op_count = match prompt_u64_in_range(
                            "Exact operation count",
                            profile.workload.exact_op_count.unwrap_or(1_000_000),
                            1,
                            1_000_000_000,
                            Some("Exact number of completed I/O operations before the run stops."),
                        )? {
                            WizardInput::Value(value) => value,
                            WizardInput::Back => continue,
                            WizardInput::Cancel => return Ok(None),
                        };
                        profile.workload.exact_op_count = Some(op_count);
                        profile.workload.runtime_seconds = None;
                    }
                    _ => unreachable!(),
                },
                WizardInput::Back => {
                    step -= 1;
                    continue;
                }
                WizardInput::Cancel => return Ok(None),
            },
            5 => match prompt_usize_in_range(
                &format!(
                    "Block size bytes [rec {}, {}-1048576, sector {} B]",
                    suggested_block_size(&profile, &hints),
                    block_size_floor(&profile, hints.logical_sector_size),
                    hints.logical_sector_size
                ),
                normalize_block_size(
                    profile.workload.block_size_bytes,
                    hints.logical_sector_size,
                    block_size_floor(&profile, hints.logical_sector_size),
                ),
                block_size_floor(&profile, hints.logical_sector_size),
                1_048_576,
                Some(
                    "Bytes per I/O request. Larger blocks usually raise throughput; direct or raw I/O requires proper alignment.",
                ),
            )? {
                WizardInput::Value(value) => profile.workload.block_size_bytes = value,
                WizardInput::Back => {
                    step -= 1;
                    continue;
                }
                WizardInput::Cancel => return Ok(None),
            },
            6 => match prompt_u32_in_range(
                "Queue depth [rec 1-8]",
                profile.workload.queue_depth.clamp(1, 8),
                1,
                64,
                Some(
                    "How many I/O requests each worker can keep in flight at once. Higher values increase concurrency and can change latency.",
                ),
            )? {
                WizardInput::Value(value) => profile.workload.queue_depth = value,
                WizardInput::Back => {
                    step -= 1;
                    continue;
                }
                WizardInput::Cancel => return Ok(None),
            },
            7 => match prompt_usize_in_range(
                &format!("Workers [rec 1-{}]", suggested_worker_count()),
                profile
                    .workload
                    .worker_count
                    .max(1)
                    .min(suggested_worker_count()),
                1,
                64,
                Some(
                    "How many worker threads issue I/O in parallel. More workers increase concurrency and can change results.",
                ),
            )? {
                WizardInput::Value(value) => profile.workload.worker_count = value,
                WizardInput::Back => {
                    step -= 1;
                    continue;
                }
                WizardInput::Cancel => return Ok(None),
            },
            8 => {
                match prompt_u64_in_range(
                    "Random seed",
                    profile.workload.random_seed,
                    1,
                    u64::MAX,
                    Some(
                        "Controls repeatable random offset selection. Use the same seed to reproduce a random pattern.",
                    ),
                )?
                {
                    WizardInput::Value(value) => profile.workload.random_seed = value,
                    WizardInput::Back => {
                        step -= 1;
                        continue;
                    }
                    WizardInput::Cancel => return Ok(None),
                }
            }
            9 => match prompt_bool_default(
                &format!(
                    "Direct I/O [rec {}]",
                    if profile.target.mode == TargetMode::RawDevice {
                        "y"
                    } else {
                        "n"
                    }
                ),
                profile.workload.direct_io,
                Some(
                    "Bypass the page cache where supported. This is closer to device behavior, but alignment rules apply and some filesystems reject it.",
                ),
            )? {
                WizardInput::Value(value) => profile.workload.direct_io = value,
                WizardInput::Back => {
                    step -= 1;
                    continue;
                }
                WizardInput::Cancel => return Ok(None),
            },
            10 => {
                if !profile.is_write_workload() {
                    profile.workload.verify = false;
                    profile.verification.enabled = false;
                    println!("Verify: off for read workloads.");
                } else {
                    match prompt_bool_default(
                        "Verify [rec n]",
                        profile.verification.enabled,
                        Some(
                            "After writes, read data back and check it. This adds safety but reduces performance.",
                        ),
                    )? {
                        WizardInput::Value(enabled) => {
                            profile.workload.verify = enabled;
                            profile.verification.enabled = enabled;
                        }
                        WizardInput::Back => {
                            step -= 1;
                            continue;
                        }
                        WizardInput::Cancel => return Ok(None),
                    }
                }
            }
            11 => {
                if !profile.is_write_workload() {
                    profile.durability.mode = DurabilityMode::Performance;
                    println!("Durability: performance for read workloads.");
                } else {
                    match prompt_menu_default_help(
                        "Durability [1=perf, 2=batch, 3=strict]",
                        durability_choice(&profile.durability.mode),
                        &["1", "2", "3"],
                        Some(
                            "performance avoids extra syncs, batch syncs periodically, and strict syncs more often for stronger durability.",
                        ),
                    )? {
                        WizardInput::Value(value) => {
                            profile.durability.mode = match value.as_str() {
                                "1" => DurabilityMode::Performance,
                                "2" => DurabilityMode::BatchDurable,
                                "3" => DurabilityMode::StrictDurable,
                                _ => unreachable!(),
                            };
                        }
                        WizardInput::Back => {
                            step -= 1;
                            continue;
                        }
                        WizardInput::Cancel => return Ok(None),
                    }
                }
            }
            12 => match prompt_bool_default(
                &format!(
                    "Health [{}]",
                    if hints.health_supported {
                        "rec y"
                    } else {
                        "rec n"
                    }
                ),
                profile.telemetry.health_telemetry && hints.health_supported,
                Some(
                    "Collect MMC or eMMC health before and after the run when the platform supports it.",
                ),
            )? {
                WizardInput::Value(value) => profile.telemetry.health_telemetry = value,
                WizardInput::Back => {
                    step -= 1;
                    continue;
                }
                WizardInput::Cancel => return Ok(None),
            },
            13 => match prompt_bool_default(
                "Diag capture [rec n]",
                profile.diagnostics.capture_during_test,
                Some(
                    "Run the lightweight I/O sampler while the workload runs so you can spot interference from other processes.",
                ),
            )? {
                WizardInput::Value(value) => profile.diagnostics.capture_during_test = value,
                WizardInput::Back => {
                    step -= 1;
                    continue;
                }
                WizardInput::Cancel => return Ok(None),
            },
            14 => match prompt_string_default_help(
                "Profile name",
                Some(&profile.name),
                Some("Name used when saving the YAML profile for later reuse."),
            )? {
                WizardInput::Value(value) => {
                    println!("  Using: {}", value);
                    profile.name = value;
                }
                WizardInput::Back => {
                    step -= 1;
                    continue;
                }
                WizardInput::Cancel => return Ok(None),
            },
            _ => {}
        }
        apply_wizard_assumptions(&mut profile, &hints);
        step += 1;
    }

    let final_hints = wizard_target_hints(&profile);
    apply_wizard_assumptions(&mut profile, &final_hints);
    profile.validate()?;
    let profile_path = infer_profile_path(paths, &profile.name);
    println!();
    println!("{}", profile.printable_summary());
    println!(
        "Equivalent command: {}",
        profile.equivalent_command(&profile_path)
    );
    loop {
        println!("1. Save as profile");
        println!("2. Run now");
        println!("3. Save and run now");
        println!("4. Back");
        println!("5. Cancel");
        match prompt_menu_default_help("Action [1-5]", "4", &["1", "2", "3", "4", "5"], None)? {
            WizardInput::Value(value) if value == "1" => {
                profile.save(&profile_path)?;
                println!("Saved profile: {}", profile_path.display());
                return Ok(Some(WizardOutcome {
                    profile,
                    run_now: false,
                }));
            }
            WizardInput::Value(value) if value == "2" => {
                return Ok(Some(WizardOutcome {
                    profile,
                    run_now: true,
                }))
            }
            WizardInput::Value(value) if value == "3" => {
                profile.save(&profile_path)?;
                println!("Saved profile: {}", profile_path.display());
                return Ok(Some(WizardOutcome {
                    profile,
                    run_now: true,
                }));
            }
            WizardInput::Value(value) if value == "4" => return wizard(paths, Some(profile)),
            WizardInput::Value(value) if value == "5" => return Ok(None),
            WizardInput::Back | WizardInput::Cancel => return Ok(None),
            WizardInput::Value(_) => unreachable!(),
        }
    }
}

fn quick_presets_flow(paths: &AppPaths, last_profile: &mut Option<Profile>) -> Result<()> {
    loop {
        let categories = preset_categories();
        let category_items = categories
            .iter()
            .map(|category| MenuItem {
                columns: vec![
                    category_label(*category).to_string(),
                    format!("{} presets", presets_in_category(*category).len()),
                ],
                detail: match category {
                    crate::presets::PresetCategory::QuickFile => {
                        "Safe file-backed canned runs using sample files under the app data directory."
                    }
                    crate::presets::PresetCategory::Partition => {
                        "Presets that resolve against a chosen partition target."
                    }
                    crate::presets::PresetCategory::RawDevice => {
                        "Presets that resolve against a whole raw block device."
                    }
                    crate::presets::PresetCategory::Focused => {
                        "Focused logical-range presets such as one-sector exact-count runs."
                    }
                    crate::presets::PresetCategory::Diagnostics => {
                        "Workload presets intended to correlate stress with diagnostic capture."
                    }
                }
                .to_string(),
            })
            .collect::<Vec<_>>();
        let Some(choice) = select_from_menu(
            "EMMC-LAB QUICK PRESETS",
            "Run Workload > Quick Presets",
            vec![("Groups".to_string(), categories.len().to_string())],
            &[
                SelectorColumn {
                    header: "Group",
                    min: 18,
                    max: 30,
                    align_right: false,
                },
                SelectorColumn {
                    header: "Count",
                    min: 10,
                    max: 12,
                    align_right: false,
                },
            ],
            &category_items,
            0,
        )?
        else {
            return Ok(());
        };
        let category = categories[choice];
        let presets = presets_in_category(category);
        let preset_items = presets
            .iter()
            .map(|preset| MenuItem {
                columns: vec![
                    preset.name.to_string(),
                    preset_scope_label(preset.target_scope).to_string(),
                    if preset.destructive {
                        "destructive".to_string()
                    } else {
                        "safe".to_string()
                    },
                ],
                detail: preset.description.to_string(),
            })
            .collect::<Vec<_>>();
        let Some(preset_choice) = select_from_menu(
            "EMMC-LAB QUICK PRESETS",
            &format!(
                "Run Workload > Quick Presets > {}",
                category_label(category)
            ),
            vec![("Group".to_string(), category_label(category).to_string())],
            &[
                SelectorColumn {
                    header: "Preset",
                    min: 18,
                    max: 30,
                    align_right: false,
                },
                SelectorColumn {
                    header: "Scope",
                    min: 10,
                    max: 12,
                    align_right: false,
                },
                SelectorColumn {
                    header: "Risk",
                    min: 10,
                    max: 12,
                    align_right: false,
                },
            ],
            &preset_items,
            0,
        )?
        else {
            continue;
        };
        let preset = presets[preset_choice];
        let mut device = None::<DeviceInfo>;
        let mut sector = None::<u64>;
        match preset.target_scope {
            PresetTargetScope::File => {}
            PresetTargetScope::Partition => {
                let devices = list_devices()?
                    .into_iter()
                    .filter(|candidate| candidate.is_partition && !is_auxiliary_device(candidate))
                    .collect::<Vec<_>>();
                if devices.is_empty() {
                    println!("No partitions discovered for this preset.");
                    continue;
                }
                let Some(path) = choose_device_path(&devices, "Select partition target", false)?
                else {
                    continue;
                };
                device = devices.into_iter().find(|candidate| candidate.path == path);
            }
            PresetTargetScope::RawDevice => {
                let devices = list_devices()?
                    .into_iter()
                    .filter(|candidate| !candidate.is_partition && !is_auxiliary_device(candidate))
                    .collect::<Vec<_>>();
                if devices.is_empty() {
                    println!("No raw block devices discovered for this preset.");
                    continue;
                }
                let Some(path) = choose_device_path(&devices, "Select raw target", false)? else {
                    continue;
                };
                device = devices.into_iter().find(|candidate| candidate.path == path);
            }
        }

        if matches!(
            preset.id,
            PresetId::OneSectorMillionReads | PresetId::OneSectorMillionWrites
        ) {
            let Some(target) = device.as_ref() else {
                println!("A block device is required for this preset.");
                continue;
            };
            let sector_size = target.logical_block_size.unwrap_or(512).max(1);
            let max_sector = target
                .size_bytes
                .unwrap_or(1024 * 1024 * 1024)
                .saturating_div(sector_size)
                .saturating_sub(1);
            sector = match prompt_u64_in_range(
                "Target sector [single logical sector]",
                0,
                0,
                max_sector,
                Some("A single logical sector to hit repeatedly. This is useful for exact-count logical-sector tests."),
            )? {
                WizardInput::Value(value) => Some(value),
                WizardInput::Back | WizardInput::Cancel => continue,
            };
        }

        let profile = resolve_preset(preset, paths, device.as_ref(), sector);
        let action_items = vec![
            MenuItem {
                columns: vec!["Run now".to_string(), "execute immediately".to_string()],
                detail: preset.description.to_string(),
            },
            MenuItem {
                columns: vec!["Edit before run".to_string(), "open in wizard".to_string()],
                detail: "Send the resolved preset into the wizard to adjust target, stop conditions, and options before execution.".to_string(),
            },
            MenuItem {
                columns: vec!["Save as profile".to_string(), "persist YAML".to_string()],
                detail: "Save the resolved preset as a normal editable YAML profile.".to_string(),
            },
            MenuItem {
                columns: vec!["Back".to_string(), "return to presets".to_string()],
                detail: "Return to the preset list.".to_string(),
            },
        ];
        let Some(action) = select_from_menu(
            "EMMC-LAB PRESET REVIEW",
            &format!("Run Workload > Quick Presets > {}", preset.name),
            vec![
                ("Preset".to_string(), preset.name.to_string()),
                (
                    "Scope".to_string(),
                    preset_scope_label(preset.target_scope).to_string(),
                ),
                (
                    "Risk".to_string(),
                    if preset.destructive {
                        "destructive"
                    } else {
                        "safe"
                    }
                    .to_string(),
                ),
                ("Profile".to_string(), profile.name.clone()),
                (
                    "Target".to_string(),
                    operator_target_label(&profile.target.path),
                ),
            ],
            &[
                SelectorColumn {
                    header: "Action",
                    min: 16,
                    max: 20,
                    align_right: false,
                },
                SelectorColumn {
                    header: "Purpose",
                    min: 18,
                    max: 32,
                    align_right: false,
                },
            ],
            &action_items,
            0,
        )?
        else {
            continue;
        };
        match action {
            0 => {
                run_profile_session_interactive(paths, profile.clone())?;
                *last_profile = Some(profile);
            }
            1 => {
                if let Some(updated) = wizard(paths, Some(profile.clone()))? {
                    if updated.run_now {
                        let final_profile = updated.profile.clone();
                        run_profile_session_interactive(paths, updated.profile)?;
                        *last_profile = Some(final_profile);
                    }
                }
            }
            2 => {
                let profile_path = infer_profile_path(paths, &profile.name);
                profile.save(&profile_path)?;
                println!("Saved profile: {}", profile_path.display());
            }
            _ => {}
        }
    }
}

fn run_saved_profile_flow(paths: &AppPaths, last_profile: &mut Option<Profile>) -> Result<()> {
    let profiles = list_profiles(paths)?;
    if profiles.is_empty() {
        println!(
            "No saved profiles found in {}",
            paths.profiles_dir.display()
        );
        return Ok(());
    }
    let items = profiles
        .iter()
        .map(|profile| MenuItem {
            columns: vec![
                profile
                    .file_stem()
                    .and_then(|value| value.to_str())
                    .unwrap_or("profile")
                    .to_string(),
                profile.display().to_string(),
            ],
            detail: format!("Saved profile at {}", profile.display()),
        })
        .collect::<Vec<_>>();
    let Some(choice) = select_from_menu(
        "EMMC-LAB SAVED PROFILES",
        "Run Workload > Saved Profiles",
        vec![("Profiles".to_string(), profiles.len().to_string())],
        &[
            SelectorColumn {
                header: "Name",
                min: 16,
                max: 28,
                align_right: false,
            },
            SelectorColumn {
                header: "Path",
                min: 24,
                max: 54,
                align_right: false,
            },
        ],
        &items,
        0,
    )?
    else {
        return Ok(());
    };
    let selected = &profiles[choice];
    let profile = Profile::load(selected)?;
    let actions = vec![
        MenuItem {
            columns: vec!["Run".to_string(), "execute saved profile".to_string()],
            detail: profile.printable_summary(),
        },
        MenuItem {
            columns: vec![
                "Edit in wizard".to_string(),
                "modify before run".to_string(),
            ],
            detail: "Open the saved profile in the guided wizard.".to_string(),
        },
        MenuItem {
            columns: vec!["Back".to_string(), "return to saved profiles".to_string()],
            detail: "Leave this profile without running it.".to_string(),
        },
    ];
    let Some(action) = select_from_menu(
        "EMMC-LAB SAVED PROFILE",
        "Run Workload > Saved Profiles",
        vec![
            ("Profile".to_string(), profile.name.clone()),
            (
                "Description".to_string(),
                profile
                    .description
                    .clone()
                    .unwrap_or_else(|| "-".to_string()),
            ),
        ],
        &[
            SelectorColumn {
                header: "Action",
                min: 16,
                max: 20,
                align_right: false,
            },
            SelectorColumn {
                header: "Purpose",
                min: 22,
                max: 36,
                align_right: false,
            },
        ],
        &actions,
        0,
    )?
    else {
        return Ok(());
    };
    match action {
        0 => {
            run_profile_session_interactive(paths, profile.clone())?;
            *last_profile = Some(profile);
        }
        1 => {
            if let Some(updated) = wizard(paths, Some(profile))? {
                if updated.run_now {
                    let profile = updated.profile.clone();
                    run_profile_session_interactive(paths, updated.profile)?;
                    *last_profile = Some(profile);
                }
            }
        }
        _ => {}
    }
    Ok(())
}

fn diagnostics_flow(paths: &AppPaths) -> Result<()> {
    loop {
        let items = vec![
            MenuItem {
                columns: vec!["Live Monitor".to_string(), "managed live screen".to_string()],
                detail: "Scope: live host activity. Risk: read-only best-effort monitor. Watch active I/O in real time with a stable screen and 1-second refresh.".to_string(),
            },
            MenuItem {
                columns: vec!["Timed Capture".to_string(), "saved sampler session".to_string()],
                detail: "Scope: local diagnostics session. Risk: read-only sampling plus local session writes. Run a procfs sampler for a fixed duration and save the results as a session.".to_string(),
            },
            MenuItem {
                columns: vec!["Deep Trace".to_string(), "best-effort trace path".to_string()],
                detail: "Scope: deeper diagnostic tracing. Risk: read-only, but capability-limited and may require elevated access. Attempt deep tracing and explain fallback behavior when tracing support is unavailable.".to_string(),
            },
            MenuItem {
                columns: vec!["Back".to_string(), "return to main menu".to_string()],
                detail: "Leave diagnostics and return to the main menu.".to_string(),
            },
        ];
        let Some(choice) = select_from_menu(
            "EMMC-LAB DIAGNOSTICS",
            "Diagnostics",
            vec![(
                "Saved Sessions".to_string(),
                paths.sessions_dir.display().to_string(),
            )],
            &[
                SelectorColumn {
                    header: "Action",
                    min: 16,
                    max: 18,
                    align_right: false,
                },
                SelectorColumn {
                    header: "Purpose",
                    min: 20,
                    max: 34,
                    align_right: false,
                },
            ],
            &items,
            0,
        )?
        else {
            return Ok(());
        };
        match choice {
            0 => {
                let duration = match prompt_u64_in_range(
                    "Monitor seconds [rec 0, 0=until q]",
                    0,
                    0,
                    86_400,
                    None,
                )? {
                    WizardInput::Value(value) => value,
                    WizardInput::Back | WizardInput::Cancel => continue,
                };
                let interval =
                    match prompt_u64_in_range("Refresh ms [rec 1000]", 1000, 250, 60_000, None)? {
                        WizardInput::Value(value) => value,
                        WizardInput::Back | WizardInput::Cancel => continue,
                    };
                run_diag_monitor(duration, interval)?;
                pause()?;
            }
            1 => timed_capture_flow(paths)?,
            2 => deep_trace_flow(paths)?,
            _ => return Ok(()),
        }
    }
}

fn timed_capture_flow(paths: &AppPaths) -> Result<()> {
    let duration = match prompt_u64_in_range("Sampler seconds [rec 60]", 60, 1, 86_400, None)? {
        WizardInput::Value(value) => value,
        WizardInput::Back | WizardInput::Cancel => return Ok(()),
    };
    let interval = match prompt_u64_in_range("Sample ms [rec 1000]", 1000, 100, 60_000, None)? {
        WizardInput::Value(value) => value,
        WizardInput::Back | WizardInput::Cancel => return Ok(()),
    };
    let _ = run_diag_sample(paths, duration, interval)?;
    Ok(())
}

fn deep_trace_flow(paths: &AppPaths) -> Result<()> {
    let duration = match prompt_u64_in_range("Trace seconds [rec 30]", 30, 1, 3_600, None)? {
        WizardInput::Value(value) => value,
        WizardInput::Back | WizardInput::Cancel => return Ok(()),
    };
    let fallback = match prompt_bool_default("Fallback to sampler?", true, None)? {
        WizardInput::Value(value) => value,
        WizardInput::Back | WizardInput::Cancel => return Ok(()),
    };
    let _ = run_diag_trace(paths, duration, fallback)?;
    Ok(())
}

fn health_flow(paths: &AppPaths) -> Result<()> {
    let devices = list_devices()?;
    let target = choose_device_path(&devices, "Select device to inspect", true)?;
    let Some(target) = target else {
        return Ok(());
    };
    run_health(paths, &target, false, None, None, 100_000)
}

fn reports_flow(paths: &AppPaths) -> Result<()> {
    loop {
        let sessions = session_menu_entries(paths)?;
        if sessions.is_empty() {
            println!("No sessions found");
            return Ok(());
        }
        let session_items = sessions
            .iter()
            .map(|session| MenuItem {
                columns: session.columns.clone(),
                detail: session.detail.clone(),
            })
            .collect::<Vec<_>>();
        let Some(choice) = select_from_menu(
            "EMMC-LAB REPORTS",
            "Reports / Export",
            vec![("Sessions".to_string(), sessions.len().to_string())],
            &[
                SelectorColumn {
                    header: "When",
                    min: 16,
                    max: 18,
                    align_right: false,
                },
                SelectorColumn {
                    header: "Type",
                    min: 10,
                    max: 12,
                    align_right: false,
                },
                SelectorColumn {
                    header: "Name",
                    min: 18,
                    max: 26,
                    align_right: false,
                },
                SelectorColumn {
                    header: "Target",
                    min: 16,
                    max: 26,
                    align_right: false,
                },
            ],
            &session_items,
            0,
        )?
        else {
            return Ok(());
        };
        let session = &sessions[choice];
        let actions = vec![
            MenuItem {
                columns: vec![
                    "View terminal summary".to_string(),
                    "screen report".to_string(),
                ],
                detail: format!(
                    "Scope: saved session {}. Risk: read-only. Render the saved session directly in the terminal.",
                    session.session_id
                ),
            },
            MenuItem {
                columns: vec!["Export JSON".to_string(), "machine-readable".to_string()],
                detail: "Scope: saved session export. Risk: writes a local JSON file under the exports directory.".to_string(),
            },
            MenuItem {
                columns: vec!["Export CSV".to_string(), "spreadsheet-friendly".to_string()],
                detail: "Scope: saved session export. Risk: writes local CSV files for summary, intervals, and processes where present."
                    .to_string(),
            },
            MenuItem {
                columns: vec!["Export HTML".to_string(), "shareable report".to_string()],
                detail: "Scope: saved session export. Risk: writes a local HTML report under the exports directory.".to_string(),
            },
            MenuItem {
                columns: vec![
                    "Delete session".to_string(),
                    "remove saved data".to_string(),
                ],
                detail: "Scope: selected saved session. Risk: destructive local delete of the session directory and exported data inside it."
                    .to_string(),
            },
            MenuItem {
                columns: vec!["Back".to_string(), "return to sessions".to_string()],
                detail: "Leave this session without viewing or exporting it.".to_string(),
            },
        ];
        let Some(action) = select_from_menu(
            "EMMC-LAB REPORT ACTIONS",
            "Reports / Export",
            vec![
                ("When".to_string(), session.columns[0].clone()),
                ("Type".to_string(), session.columns[1].clone()),
                ("Name".to_string(), session.columns[2].clone()),
                ("Target".to_string(), session.columns[3].clone()),
            ],
            &[
                SelectorColumn {
                    header: "Action",
                    min: 20,
                    max: 24,
                    align_right: false,
                },
                SelectorColumn {
                    header: "Purpose",
                    min: 18,
                    max: 32,
                    align_right: false,
                },
            ],
            &actions,
            0,
        )?
        else {
            continue;
        };
        match action {
            0 => {
                run_report(paths, &session.session_id)?;
                pause()?;
            }
            1 => {
                run_export(paths, &session.session_id, ExportFormat::Json)?;
                pause()?;
            }
            2 => {
                run_export(paths, &session.session_id, ExportFormat::Csv)?;
                pause()?;
            }
            3 => {
                run_export(paths, &session.session_id, ExportFormat::Html)?;
                pause()?;
            }
            4 => match prompt_bool_default(
                &format!("Delete session {} [rec n]", session.columns[2]),
                false,
                Some("Removes the saved session directory, including its summary and interval files."),
            )? {
                WizardInput::Value(true) => {
                    delete_session(paths, &session.session_id)?;
                    println!("Deleted session {}", session.session_id);
                    pause()?;
                }
                WizardInput::Value(false) | WizardInput::Back | WizardInput::Cancel => {}
            },
            _ => continue,
        }
    }
}

fn session_menu_entries(paths: &AppPaths) -> Result<Vec<SessionMenuEntry>> {
    let mut entries = Vec::new();
    for session_id in list_sessions(paths)? {
        let record = load_session(paths, &session_id)?;
        let created = record
            .created_at
            .with_timezone(&Local)
            .format("%Y-%m-%d %H:%M")
            .to_string();
        let kind = session_kind_label(&record);
        let name = session_name_label(&record);
        let target = session_target_label(&record);
        let detail = format!(
            "{} session {} on {}",
            kind,
            session_id,
            record.system.hostname.unwrap_or_else(|| "-".to_string())
        );
        entries.push(SessionMenuEntry {
            session_id,
            columns: vec![created, kind.to_string(), name, target],
            detail,
        });
    }
    Ok(entries)
}

fn session_kind_label(record: &SessionRecord) -> &'static str {
    if record.run_summary.is_some() {
        "run"
    } else if record.diagnostics.is_some() {
        "diag"
    } else {
        "other"
    }
}

fn session_name_label(record: &SessionRecord) -> String {
    if let Some(profile) = &record.profile {
        return profile.name.clone();
    }
    if let Some(diag) = &record.diagnostics {
        return format!("{:?}", diag.mode).to_lowercase().replace('_', "-");
    }
    "unnamed".to_string()
}

fn session_target_label(record: &SessionRecord) -> String {
    if let Some(run) = &record.run_summary {
        return short_target_label(&run.target_path);
    }
    if let Some(profile) = &record.profile {
        return short_target_label(&profile.target.path);
    }
    "-".to_string()
}

fn short_target_label(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| path.display().to_string())
}

fn operator_target_label(path: &Path) -> String {
    let path_text = path.display().to_string();
    let Some(device) = list_devices()
        .ok()
        .and_then(|devices| devices.into_iter().find(|device| device.path == path))
    else {
        return path_text;
    };

    if device.is_partition {
        if let Some(mount) = device.mountpoints.first().filter(|value| !value.is_empty()) {
            return format!("{path_text} [partition {mount}]");
        }
        if let Some(parent) = device.parent.as_deref().filter(|value| !value.is_empty()) {
            return format!("{path_text} [partition of {parent}]");
        }
        return format!("{path_text} [partition]");
    }

    if let Some(model) = device
        .model
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        return format!("{path_text} [device {model}]");
    }

    match device.media_type.as_deref() {
        Some(kind) if !kind.trim().is_empty() => format!("{path_text} [device {kind}]"),
        _ => format!("{path_text} [device]"),
    }
}

fn run_profile_session_interactive(paths: &AppPaths, profile: Profile) -> Result<()> {
    loop {
        let session_id = run_profile_session(paths, profile.clone())?;
        println!("1. Repeat same test");
        println!("2. View this report");
        println!("3. Back");
        match prompt_menu_default_help("Action [1-3]", "3", &["1", "2", "3"], None)? {
            WizardInput::Value(value) if value == "1" => continue,
            WizardInput::Value(value) if value == "2" => {
                run_report(paths, &session_id)?;
                pause()?;
            }
            _ => return Ok(()),
        }
    }
}

fn settings_flow(paths: &AppPaths) -> Result<()> {
    let caps = collect_capabilities(paths);
    let system = collect_system_snapshot();
    println!("{}", render_settings(paths, &system, &caps));
    Ok(())
}

fn print_help(paths: &AppPaths) {
    println!("emmc-lab Help");
    println!("=============");
    println!();
    println!("Start");
    println!("-----");
    println!("Interactive menu: emmc-lab");
    println!("Direct wizard:    emmc-lab wizard");
    println!(
        "Saved profile:    emmc-lab run --profile {}",
        paths.profiles_dir.join("example.yaml").display()
    );
    println!();
    println!("Workloads");
    println!("---------");
    println!("Quick presets:    Run Workload -> Quick Presets");
    println!("Custom run:       Run Workload -> Guided Custom Run");
    println!("Repeat last run:  Run Workload -> Repeat Last Run");
    println!("1M logical write: emmc-lab run --profile /path/to/million-writes.yaml --i-understand-this-will-destroy-data");
    println!();
    println!("Diagnostics");
    println!("-----------");
    println!("Live monitor:     emmc-lab diag monitor --interval-ms 1000");
    println!("Timed sampler:    emmc-lab diag sample --duration 60");
    println!("Deep trace:       emmc-lab diag trace --duration 30");
    println!("Run + capture:    enable diagnostic capture in the wizard or YAML profile");
    println!();
    println!("Health And Reports");
    println!("------------------");
    println!("Health check:     emmc-lab health --device /dev/<device>");
    println!("View report:      emmc-lab report --session <id>");
    println!("Export HTML:      emmc-lab export --session <id> --format html");
    println!("Doctor:           emmc-lab doctor");
    println!("List devices:     emmc-lab list-devices");
    println!();
    println!("Wizard Tips");
    println!("-----------");
    println!("Enter keeps the default.");
    println!("Type ? for a short field explanation.");
    println!("Type back or cancel to leave a step.");
    println!("Raw-device writes are destructive and require explicit confirmation.");
}

fn derive_interference_note(diag: &DiagnosticReport) -> Option<String> {
    let me = process::id() as i32;
    let noisy = diag
        .top_processes
        .iter()
        .filter(|p| p.pid != me)
        .filter(|p| p.storage_read_bytes_per_sec > 0.0 || p.storage_write_bytes_per_sec > 0.0)
        .take(5)
        .map(|p| {
            format!(
                "pid={} cmd={} share={:.1}%",
                p.pid, p.command, p.observed_share_percent
            )
        })
        .collect::<Vec<_>>();
    if noisy.is_empty() {
        None
    } else {
        Some(format!(
            "Possible interference from concurrent I/O producers: {}",
            noisy.join("; ")
        ))
    }
}

fn session_id() -> String {
    format!(
        "{}-{}",
        Utc::now().format("%Y%m%dT%H%M%S"),
        Uuid::new_v4().simple()
    )
}

fn prompt_string_default(label: &str, default: Option<&str>) -> Result<WizardInput<String>> {
    match prompt_string_default_help(label, default, None)? {
        WizardInput::Value(value) => {
            println!("  Using: {}", value);
            Ok(WizardInput::Value(value))
        }
        WizardInput::Back => Ok(WizardInput::Back),
        WizardInput::Cancel => Ok(WizardInput::Cancel),
    }
}

fn prompt_string_default_help(
    label: &str,
    default: Option<&str>,
    help: Option<&str>,
) -> Result<WizardInput<String>> {
    prompt_string_default_help_seeded(label, default, None, help)
}

fn prompt_string_default_help_seeded(
    label: &str,
    default: Option<&str>,
    initial: Option<&str>,
    help: Option<&str>,
) -> Result<WizardInput<String>> {
    loop {
        if let Some(help) = help {
            println!("Help: {}", help);
        }
        let prompt = match default {
            Some(default) => format!("{} [{}]: ", label, default),
            None => format!("{}: ", label),
        };
        let Some(input) = read_prompt_value(&prompt, initial)? else {
            return Ok(WizardInput::Cancel);
        };
        let trimmed = input.trim();
        if trimmed.eq_ignore_ascii_case("back") {
            return Ok(WizardInput::Back);
        }
        if trimmed.eq_ignore_ascii_case("cancel") {
            return Ok(WizardInput::Cancel);
        }
        if trimmed == "?" {
            if help.is_some() {
                continue;
            }
            println!("No extra help is available for this field.");
            continue;
        }
        if trimmed.is_empty() {
            if let Some(default) = default {
                return Ok(WizardInput::Value(default.to_string()));
            }
            println!("Input cannot be empty");
            continue;
        }
        return Ok(WizardInput::Value(trimmed.to_string()));
    }
}

fn prompt_menu_default_help(
    label: &str,
    default: &str,
    allowed: &[&str],
    help: Option<&str>,
) -> Result<WizardInput<String>> {
    let mut retry_value = None::<String>;
    loop {
        match prompt_string_default_help_seeded(label, Some(default), retry_value.as_deref(), help)?
        {
            WizardInput::Value(value) if allowed.contains(&value.as_str()) => {
                println!("  Using: {}", value);
                return Ok(WizardInput::Value(value));
            }
            WizardInput::Value(value) => {
                retry_value = Some(value);
                println!("Invalid choice. Enter one of: {}", allowed.join(", "));
            }
            WizardInput::Back => return Ok(WizardInput::Back),
            WizardInput::Cancel => return Ok(WizardInput::Cancel),
        }
    }
}

fn prompt_u64_in_range(
    label: &str,
    default: u64,
    min: u64,
    max: u64,
    help: Option<&str>,
) -> Result<WizardInput<u64>> {
    let default_text = default.to_string();
    let mut retry_value = None::<String>;
    loop {
        let prompt_label = format!("{label} [{min}-{max}]");
        match prompt_string_default_help_seeded(
            &prompt_label,
            Some(&default_text),
            retry_value.as_deref(),
            help,
        )? {
            WizardInput::Value(value) => match value.parse::<u64>() {
                Ok(parsed) if parsed >= min && parsed <= max => {
                    println!("  Using: {}", parsed);
                    return Ok(WizardInput::Value(parsed));
                }
                Ok(_) => {
                    retry_value = Some(value);
                    println!("Value must be between {min} and {max}");
                }
                Err(err) => {
                    retry_value = Some(value);
                    println!("Invalid value: {}", err);
                }
            },
            WizardInput::Back => return Ok(WizardInput::Back),
            WizardInput::Cancel => return Ok(WizardInput::Cancel),
        }
    }
}

fn prompt_u32_in_range(
    label: &str,
    default: u32,
    min: u32,
    max: u32,
    help: Option<&str>,
) -> Result<WizardInput<u32>> {
    match prompt_u64_in_range(label, default as u64, min as u64, max as u64, help)? {
        WizardInput::Value(value) => Ok(WizardInput::Value(value as u32)),
        WizardInput::Back => Ok(WizardInput::Back),
        WizardInput::Cancel => Ok(WizardInput::Cancel),
    }
}

fn prompt_usize_in_range(
    label: &str,
    default: usize,
    min: usize,
    max: usize,
    help: Option<&str>,
) -> Result<WizardInput<usize>> {
    match prompt_u64_in_range(label, default as u64, min as u64, max as u64, help)? {
        WizardInput::Value(value) => Ok(WizardInput::Value(value as usize)),
        WizardInput::Back => Ok(WizardInput::Back),
        WizardInput::Cancel => Ok(WizardInput::Cancel),
    }
}

fn prompt_bool_default(
    label: &str,
    default: bool,
    help: Option<&str>,
) -> Result<WizardInput<bool>> {
    let default_text = if default { "y" } else { "n" };
    let mut retry_value = None::<String>;
    loop {
        match prompt_string_default_help_seeded(
            &format!("{label} [y/n]"),
            Some(default_text),
            retry_value.as_deref(),
            help,
        )? {
            WizardInput::Value(value) => match parse_yes_no(&value) {
                Ok(parsed) => {
                    println!("  Using: {}", if parsed { "y" } else { "n" });
                    return Ok(WizardInput::Value(parsed));
                }
                Err(_) => {
                    retry_value = Some(value);
                    println!("Expected y or n");
                }
            },
            WizardInput::Back => return Ok(WizardInput::Back),
            WizardInput::Cancel => return Ok(WizardInput::Cancel),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PromptKey {
    Char(char),
    Enter,
    Backspace,
    Delete,
    Escape,
    Left,
    Right,
    Home,
    End,
    Cancel,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PromptBuffer {
    text: String,
    cursor: usize,
}

impl PromptBuffer {
    fn from_seed(seed: Option<&str>) -> Self {
        let text = seed.unwrap_or_default().to_string();
        let cursor = text.chars().count();
        Self { text, cursor }
    }

    fn insert(&mut self, ch: char) {
        let byte = self.byte_index(self.cursor);
        self.text.insert(byte, ch);
        self.cursor += 1;
    }

    fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let end = self.byte_index(self.cursor);
        let start = self.byte_index(self.cursor - 1);
        self.text.replace_range(start..end, "");
        self.cursor -= 1;
    }

    fn delete(&mut self) {
        if self.cursor >= self.len() {
            return;
        }
        let start = self.byte_index(self.cursor);
        let end = self.byte_index(self.cursor + 1);
        self.text.replace_range(start..end, "");
    }

    fn move_left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    fn move_right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.len());
    }

    fn move_home(&mut self) {
        self.cursor = 0;
    }

    fn move_end(&mut self) {
        self.cursor = self.len();
    }

    fn len(&self) -> usize {
        self.text.chars().count()
    }

    fn cursor_offset(&self, prompt_width: usize) -> usize {
        prompt_width + self.cursor
    }

    fn byte_index(&self, char_index: usize) -> usize {
        if char_index == self.len() {
            return self.text.len();
        }
        self.text
            .char_indices()
            .nth(char_index)
            .map(|(idx, _)| idx)
            .unwrap_or(self.text.len())
    }
}

struct PromptTerminalGuard {
    original: libc::termios,
    active: bool,
}

impl PromptTerminalGuard {
    fn new() -> Result<Self> {
        if unsafe { libc::isatty(libc::STDIN_FILENO) } != 1
            || unsafe { libc::isatty(libc::STDOUT_FILENO) } != 1
        {
            return Ok(Self {
                original: unsafe { std::mem::zeroed() },
                active: false,
            });
        }
        let mut original = MaybeUninit::<libc::termios>::uninit();
        if unsafe { libc::tcgetattr(libc::STDIN_FILENO, original.as_mut_ptr()) } != 0 {
            bail!("failed to read terminal mode");
        }
        let original = unsafe { original.assume_init() };
        let mut raw = original;
        raw.c_lflag &= !(libc::ICANON | libc::ECHO);
        raw.c_cc[libc::VMIN] = 0;
        raw.c_cc[libc::VTIME] = 0;
        if unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &raw) } != 0 {
            bail!("failed to set prompt terminal mode");
        }
        Ok(Self {
            original,
            active: true,
        })
    }
}

impl Drop for PromptTerminalGuard {
    fn drop(&mut self) {
        if self.active {
            unsafe {
                libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &self.original);
            }
        }
    }
}

fn read_prompt_value(prompt: &str, initial: Option<&str>) -> Result<Option<String>> {
    if unsafe { libc::isatty(libc::STDIN_FILENO) } != 1
        || unsafe { libc::isatty(libc::STDOUT_FILENO) } != 1
    {
        print!("{prompt}");
        io::stdout().flush()?;
        let mut input = String::new();
        if io::stdin().read_line(&mut input)? == 0 {
            return Ok(None);
        }
        return Ok(Some(input));
    }

    let _terminal = PromptTerminalGuard::new()?;
    let mut buffer = PromptBuffer::from_seed(initial);
    render_prompt_line(prompt, &buffer)?;
    loop {
        match read_prompt_key()? {
            PromptKey::Char(ch) => buffer.insert(ch),
            PromptKey::Backspace => buffer.backspace(),
            PromptKey::Delete => buffer.delete(),
            PromptKey::Escape => {}
            PromptKey::Left => buffer.move_left(),
            PromptKey::Right => buffer.move_right(),
            PromptKey::Home => buffer.move_home(),
            PromptKey::End => buffer.move_end(),
            PromptKey::Enter => {
                println!();
                io::stdout().flush()?;
                return Ok(Some(buffer.text));
            }
            PromptKey::Cancel => {
                println!();
                io::stdout().flush()?;
                return Ok(None);
            }
        }
        render_prompt_line(prompt, &buffer)?;
    }
}

fn render_prompt_line(prompt: &str, buffer: &PromptBuffer) -> Result<()> {
    write!(io::stdout(), "\r\u{1b}[K{}{}", prompt, buffer.text)?;
    let cursor = buffer.cursor_offset(prompt.chars().count());
    if cursor > 0 {
        write!(io::stdout(), "\r\u{1b}[{}C", cursor)?;
    } else {
        write!(io::stdout(), "\r")?;
    }
    io::stdout().flush()?;
    Ok(())
}

fn read_prompt_key() -> Result<PromptKey> {
    loop {
        let Some(byte) = poll_keypress_until(Duration::from_secs(60))? else {
            continue;
        };
        return match byte {
            b'\r' | b'\n' => Ok(PromptKey::Enter),
            3 | 4 => Ok(PromptKey::Cancel),
            8 | 127 => Ok(PromptKey::Backspace),
            1 => Ok(PromptKey::Home),
            5 => Ok(PromptKey::End),
            27 => parse_prompt_escape_sequence(),
            b' '..=b'~' => Ok(PromptKey::Char(byte as char)),
            _ => continue,
        };
    }
}

fn parse_prompt_escape_sequence() -> Result<PromptKey> {
    let Some(next) = poll_keypress_until(Duration::from_millis(150))? else {
        return Ok(PromptKey::Escape);
    };
    if next != b'[' {
        return Ok(PromptKey::Escape);
    }
    let Some(code) = poll_keypress_until(Duration::from_millis(150))? else {
        return Ok(PromptKey::Escape);
    };
    match code {
        b'C' => Ok(PromptKey::Right),
        b'D' => Ok(PromptKey::Left),
        b'H' => Ok(PromptKey::Home),
        b'F' => Ok(PromptKey::End),
        b'1' | b'3' | b'4' | b'7' | b'8' => {
            let Some(suffix) = poll_keypress_until(Duration::from_millis(150))? else {
                return Ok(PromptKey::Escape);
            };
            match (code, suffix) {
                (b'1', b'~') | (b'7', b'~') => Ok(PromptKey::Home),
                (b'3', b'~') => Ok(PromptKey::Delete),
                (b'4', b'~') | (b'8', b'~') => Ok(PromptKey::End),
                _ => Ok(PromptKey::Escape),
            }
        }
        _ => Ok(PromptKey::Escape),
    }
}

fn prompt_target_path(mode: &TargetMode) -> Result<WizardInput<String>> {
    match mode {
        TargetMode::RawDevice => {
            let devices = preferred_devices(list_devices()?);
            let selected = choose_device_path(&devices, "Select raw target device", true)?;
            match selected {
                Some(path) => Ok(WizardInput::Value(path.display().to_string())),
                None => Ok(WizardInput::Back),
            }
        }
        TargetMode::FileBased => choose_file_target_path(),
    }
}

fn choose_file_target_path() -> Result<WizardInput<String>> {
    let mut options = Vec::new();
    options.push(PathBuf::from("/tmp/emmc-lab.bin"));
    if let Ok(home) = env::var("HOME") {
        options.push(PathBuf::from(home).join("emmc-lab.bin"));
    }
    if let Ok(cwd) = env::current_dir() {
        options.push(cwd.join("emmc-lab.bin"));
    }
    options.dedup();

    let mut items = options
        .iter()
        .enumerate()
        .map(|(index, option)| {
            let size = fs::metadata(option)
                .ok()
                .filter(|meta| meta.is_file())
                .map(|meta| format_bytes(meta.len()))
                .unwrap_or_else(|| "-".to_string());
            MenuItem {
                columns: vec![
                    option.display().to_string(),
                    size,
                    if index == 0 {
                        "default".to_string()
                    } else {
                        "-".to_string()
                    },
                ],
                detail: format!(
                    "Use {} as the file-backed workload target.",
                    option.display()
                ),
            }
        })
        .collect::<Vec<_>>();
    items.push(MenuItem {
        columns: vec![
            "Enter path manually".to_string(),
            "-".to_string(),
            "-".to_string(),
        ],
        detail: "Type a custom file path.".to_string(),
    });
    let Some(choice) = select_from_menu(
        "EMMC-LAB FILE TARGET",
        "Wizard > Select File Target",
        vec![("Suggestions".to_string(), options.len().to_string())],
        &[
            SelectorColumn {
                header: "Path",
                min: 24,
                max: 48,
                align_right: false,
            },
            SelectorColumn {
                header: "Size",
                min: 10,
                max: 18,
                align_right: false,
            },
            SelectorColumn {
                header: "Hint",
                min: 8,
                max: 10,
                align_right: false,
            },
        ],
        &items,
        0,
    )?
    else {
        return Ok(WizardInput::Back);
    };
    if choice < options.len() {
        return Ok(WizardInput::Value(options[choice].display().to_string()));
    }
    match prompt_string_default("File path", Some(&options[0].display().to_string()))? {
        WizardInput::Value(value) => Ok(WizardInput::Value(value)),
        WizardInput::Back => Ok(WizardInput::Back),
        WizardInput::Cancel => Ok(WizardInput::Cancel),
    }
}

fn choose_device_path(
    devices: &[DeviceInfo],
    title: &str,
    allow_manual: bool,
) -> Result<Option<PathBuf>> {
    let devices = preferred_devices(devices.to_vec());
    let primary_devices = devices
        .iter()
        .filter(|device| !is_auxiliary_device(device))
        .cloned()
        .collect::<Vec<_>>();
    let mut show_all = primary_devices.is_empty();
    loop {
        let visible_devices = if show_all { &devices } else { &primary_devices };
        let mut items = visible_devices
            .iter()
            .map(|device| MenuItem {
                columns: vec![
                    device.path.display().to_string(),
                    if device.is_partition {
                        "partition".to_string()
                    } else {
                        "device".to_string()
                    },
                    device
                        .size_bytes
                        .map(format_bytes)
                        .unwrap_or_else(|| "-".to_string()),
                ],
                detail: device_selection_label(device),
            })
            .collect::<Vec<_>>();
        let show_all_index = if !show_all && primary_devices.len() < devices.len() {
            items.push(MenuItem {
                columns: vec![
                    "Show all devices".to_string(),
                    "action".to_string(),
                    "-".to_string(),
                ],
                detail: "Expand the device list to include loop and ram devices.".to_string(),
            });
            Some(items.len() - 1)
        } else {
            None
        };
        let manual_index = if allow_manual {
            items.push(MenuItem {
                columns: vec![
                    "Enter path manually".to_string(),
                    "action".to_string(),
                    "-".to_string(),
                ],
                detail: "Type a custom device path.".to_string(),
            });
            Some(items.len() - 1)
        } else {
            None
        };
        let Some(choice) = select_from_menu(
            "EMMC-LAB DEVICE PICKER",
            title,
            vec![
                ("Visible".to_string(), visible_devices.len().to_string()),
                (
                    "Mode".to_string(),
                    if show_all {
                        "all devices"
                    } else {
                        "preferred only"
                    }
                    .to_string(),
                ),
            ],
            &[
                SelectorColumn {
                    header: "Path",
                    min: 18,
                    max: 28,
                    align_right: false,
                },
                SelectorColumn {
                    header: "Kind",
                    min: 10,
                    max: 12,
                    align_right: false,
                },
                SelectorColumn {
                    header: "Size",
                    min: 10,
                    max: 18,
                    align_right: false,
                },
            ],
            &items,
            0,
        )?
        else {
            return Ok(None);
        };
        if choice < visible_devices.len() {
            return Ok(Some(visible_devices[choice].path.clone()));
        }
        if show_all_index == Some(choice) {
            show_all = true;
            continue;
        }
        if manual_index == Some(choice) {
            match prompt_string_default("Device path", None)? {
                WizardInput::Value(value) => return Ok(Some(PathBuf::from(value))),
                WizardInput::Back => continue,
                WizardInput::Cancel => return Ok(None),
            }
        }
    }
}

fn preferred_devices(mut devices: Vec<DeviceInfo>) -> Vec<DeviceInfo> {
    devices.sort_by(|a, b| {
        device_rank(a)
            .cmp(&device_rank(b))
            .then_with(|| a.name.cmp(&b.name))
    });
    devices
}

fn device_rank(device: &DeviceInfo) -> (u8, u8, u8) {
    let name = device.name.as_str();
    let family = if name.starts_with("mmcblk") {
        0
    } else if name.starts_with("nvme") || name.starts_with("sd") {
        1
    } else if name.starts_with("loop") {
        3
    } else if name.starts_with("ram") {
        4
    } else {
        2
    };
    let mounted = if device.mounted { 0 } else { 1 };
    let root = if device.is_root_device { 1 } else { 0 };
    (family, mounted, root)
}

fn is_auxiliary_device(device: &DeviceInfo) -> bool {
    device.name.starts_with("loop") || device.name.starts_with("ram")
}

fn preferred_raw_device_path() -> Option<PathBuf> {
    let mut devices = list_devices().ok()?;
    devices.sort_by_key(device_rank);
    devices
        .iter()
        .find(|device| !device.is_partition && !is_auxiliary_device(device))
        .or_else(|| devices.iter().find(|device| !is_auxiliary_device(device)))
        .map(|device| device.path.clone())
}

fn default_target_path(mode: &TargetMode) -> PathBuf {
    match mode {
        TargetMode::RawDevice => {
            preferred_raw_device_path().unwrap_or_else(|| PathBuf::from("/dev/mmcblk0"))
        }
        TargetMode::FileBased => PathBuf::from("/tmp/emmc-lab.bin"),
    }
}

fn wizard_target_hints(profile: &Profile) -> WizardTargetHints {
    match profile.target.mode {
        TargetMode::RawDevice => {
            let device = list_devices().ok().and_then(|devices| {
                devices
                    .into_iter()
                    .find(|device| device.path == profile.target.path)
            });
            let size_bytes = device
                .as_ref()
                .and_then(|device| device.size_bytes)
                .or_else(|| block_device_size(&profile.target.path));
            let logical_sector_size = device
                .as_ref()
                .and_then(|device| device.logical_block_size)
                .or_else(|| logical_block_size(&profile.target.path))
                .unwrap_or(512);
            let media = device
                .as_ref()
                .and_then(|device| device.media_type.clone())
                .unwrap_or_else(|| device_family_name(&profile.target.path).to_string());
            let model = device
                .as_ref()
                .and_then(|device| device.model.clone())
                .unwrap_or_default();
            let size = size_bytes
                .map(format_bytes)
                .unwrap_or_else(|| "size unknown".to_string());
            let model_suffix = if model.is_empty() {
                String::new()
            } else {
                format!(", model {model}")
            };
            WizardTargetHints {
                target_label: format!(
                    "{} [{}{}, {}, sector {} B]",
                    profile.target.path.display(),
                    media,
                    model_suffix,
                    size,
                    logical_sector_size
                ),
                size_bytes,
                logical_sector_size,
                health_supported: profile.target.path.to_string_lossy().contains("mmcblk"),
            }
        }
        TargetMode::FileBased => {
            let metadata = fs::metadata(&profile.target.path).ok();
            let existing_size = metadata
                .as_ref()
                .filter(|meta| meta.is_file())
                .map(|meta| meta.len());
            let size_bytes = existing_size.or(profile.target.create_if_missing_size_bytes);
            let status = if metadata.as_ref().is_some_and(|meta| meta.is_dir()) {
                "directory path"
            } else if existing_size.is_some() {
                "existing file"
            } else if profile.target.create_if_missing_size_bytes.is_some() {
                "runtime-created if missing"
            } else {
                "new file"
            };
            WizardTargetHints {
                target_label: format!(
                    "{} [file target, {}, {}, sector 512 B]",
                    profile.target.path.display(),
                    status,
                    size_bytes
                        .map(format_bytes)
                        .unwrap_or_else(|| "size will be created as needed".to_string())
                ),
                size_bytes,
                logical_sector_size: 512,
                health_supported: false,
            }
        }
    }
}

fn normalize_file_target_path(mode: &TargetMode, path: PathBuf) -> PathBuf {
    if *mode != TargetMode::FileBased {
        return path;
    }
    if path.is_dir() {
        return path.join("emmc-lab.bin");
    }
    path
}

fn apply_wizard_assumptions(profile: &mut Profile, hints: &WizardTargetHints) {
    profile.target.path =
        normalize_file_target_path(&profile.target.mode, profile.target.path.clone());
    if profile.target.mode == TargetMode::FileBased {
        if profile.target.path.exists() {
            profile.target.create_if_missing_size_bytes = None;
        } else {
            let minimum = minimum_runtime_file_size(profile, hints.logical_sector_size);
            let suggested = suggested_runtime_file_size(profile, hints);
            let selected = profile
                .target
                .create_if_missing_size_bytes
                .unwrap_or(suggested)
                .max(minimum);
            profile.target.create_if_missing_size_bytes = Some(selected);
        }
    } else {
        profile.target.create_if_missing_size_bytes = None;
    }
    if !profile.is_write_workload() {
        profile.workload.verify = false;
        profile.verification.enabled = false;
        profile.durability.mode = DurabilityMode::Performance;
    }
    let normalized = adjusted_block_size(profile, hints.logical_sector_size);
    if normalized != profile.workload.block_size_bytes {
        println!(
            "Adjusted block size from {} to {} for target alignment.",
            profile.workload.block_size_bytes, normalized
        );
        profile.workload.block_size_bytes = normalized;
    }
    if !hints.health_supported {
        profile.telemetry.health_telemetry = false;
    }
    if profile.name.trim().is_empty() {
        profile.name = default_profile_name(profile);
    }
}

fn block_size_floor(profile: &Profile, logical_sector_size: u64) -> usize {
    let sector = logical_sector_size.max(512) as usize;
    if matches!(profile.addressing.mode, AddressingMode::SectorRange) {
        sector
    } else {
        sector.max(4096)
    }
}

fn normalize_block_size(current: usize, logical_sector_size: u64, floor: usize) -> usize {
    let sector = logical_sector_size.max(512) as usize;
    let floor = floor.max(sector);
    if current == 0 {
        return floor;
    }
    if current < floor {
        return floor;
    }
    let rounded = ((current + sector - 1) / sector) * sector;
    rounded.max(floor)
}

fn suggested_block_size(profile: &Profile, hints: &WizardTargetHints) -> usize {
    let base = normalize_block_size(
        4096,
        hints.logical_sector_size,
        block_size_floor(profile, hints.logical_sector_size),
    );
    sector_aligned_block_size(profile, hints.logical_sector_size, base)
}

#[cfg(test)]
fn is_single_sector_target(profile: &Profile) -> bool {
    matches!(profile.addressing.mode, AddressingMode::SectorRange)
        && profile.addressing.sector_count == Some(1)
}

fn adjusted_block_size(profile: &Profile, logical_sector_size: u64) -> usize {
    let normalized = normalize_block_size(
        profile.workload.block_size_bytes,
        logical_sector_size,
        block_size_floor(profile, logical_sector_size),
    );
    sector_aligned_block_size(profile, logical_sector_size, normalized)
}

fn sector_aligned_block_size(
    profile: &Profile,
    logical_sector_size: u64,
    requested: usize,
) -> usize {
    if !matches!(profile.addressing.mode, AddressingMode::SectorRange) {
        return requested;
    }
    let sector = logical_sector_size.max(512);
    let start_sector = profile.addressing.start_sector.unwrap_or(0);
    let sector_count = profile.addressing.sector_count.unwrap_or(0).max(1);
    let start_bytes = start_sector.saturating_mul(sector);
    let range_bytes = sector_count.saturating_mul(sector);
    let floor = block_size_floor(profile, logical_sector_size) as u64;
    let mut candidate = ((requested as u64) / sector).max(1).saturating_mul(sector);
    while candidate >= floor {
        if candidate <= range_bytes && start_bytes % candidate == 0 {
            return candidate as usize;
        }
        if candidate <= sector {
            break;
        }
        candidate = candidate.saturating_sub(sector);
    }
    sector as usize
}

fn suggested_worker_count() -> usize {
    std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1)
        .clamp(1, 8)
}

fn minimum_runtime_file_size(profile: &Profile, logical_sector_size: u64) -> u64 {
    let sector = logical_sector_size.max(512);
    let block = profile
        .workload
        .block_size_bytes
        .max(block_size_floor(profile, logical_sector_size)) as u64;
    let required = match profile.addressing.mode {
        AddressingMode::WholeSelectedRange => 64 * 1024 * 1024,
        AddressingMode::SectorRange => profile
            .addressing
            .start_sector
            .unwrap_or(0)
            .saturating_add(profile.addressing.sector_count.unwrap_or(0))
            .saturating_mul(sector),
        AddressingMode::ByteRange => profile
            .addressing
            .start_offset_bytes
            .unwrap_or(0)
            .saturating_add(profile.addressing.range_size_bytes.unwrap_or(0)),
    };
    required.max(block).max(sector)
}

fn suggested_runtime_file_size(profile: &Profile, hints: &WizardTargetHints) -> u64 {
    let minimum = minimum_runtime_file_size(profile, hints.logical_sector_size);
    if let Some(existing) = hints.size_bytes {
        return existing.max(minimum);
    }
    let safe_free = maximum_runtime_file_size(&profile.target.path)
        .map(|bytes| (bytes / 10).max(minimum))
        .unwrap_or(1024 * 1024 * 1024);
    safe_free.clamp(minimum, (4 * 1024 * 1024 * 1024_u64).max(minimum))
}

fn maximum_runtime_file_size(path: &Path) -> Option<u64> {
    let base = if path.exists() {
        path
    } else {
        path.parent().unwrap_or(Path::new("."))
    };
    available_space_bytes(base)
}

fn available_space_bytes(path: &Path) -> Option<u64> {
    let c_path = CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::zeroed();
    let rc = unsafe { libc::statvfs(c_path.as_ptr(), stat.as_mut_ptr()) };
    if rc != 0 {
        return None;
    }
    let stat = unsafe { stat.assume_init() };
    Some((stat.f_bavail as u64).saturating_mul(stat.f_frsize as u64))
}

fn range_hint_label(hints: &WizardTargetHints) -> String {
    hints
        .size_bytes
        .map(|size| format!("target {}", format_bytes(size)))
        .unwrap_or_else(|| "target size discovered at run time".to_string())
}

fn preset_scope_label(scope: PresetTargetScope) -> &'static str {
    match scope {
        PresetTargetScope::File => "file",
        PresetTargetScope::Partition => "partition",
        PresetTargetScope::RawDevice => "raw",
    }
}

fn device_selection_label(device: &DeviceInfo) -> String {
    let target_kind = if device.is_partition {
        match device.parent.as_deref() {
            Some(parent) => format!("partition of {parent}"),
            None => "partition".to_string(),
        }
    } else {
        device
            .media_type
            .clone()
            .unwrap_or_else(|| device_family_name(&device.path).to_string())
    };
    let mount_info = if device.mountpoints.is_empty() {
        "not mounted".to_string()
    } else {
        device
            .mountpoints
            .iter()
            .enumerate()
            .map(|(index, mountpoint)| {
                let fs_type = device
                    .filesystem_types
                    .get(index)
                    .map(String::as_str)
                    .unwrap_or("?");
                let options = device
                    .mount_options
                    .get(index)
                    .map(String::as_str)
                    .unwrap_or("");
                if options.is_empty() {
                    format!("{mountpoint} [{fs_type}]")
                } else {
                    format!("{mountpoint} [{fs_type} {options}]")
                }
            })
            .collect::<Vec<_>>()
            .join(", ")
    };
    format!(
        "{} [{}{}, sector {} B, {}, root={}]",
        device.path.display(),
        target_kind,
        device
            .size_bytes
            .map(|size| format!(", {}", format_bytes(size)))
            .unwrap_or_default(),
        device.logical_block_size.unwrap_or(512),
        mount_info,
        device.is_root_device
    )
}

fn device_family_name(path: &Path) -> &'static str {
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    if name.starts_with("mmcblk") {
        "MMC/SD"
    } else if name.starts_with("nvme") {
        "NVMe"
    } else if name.starts_with("sd") {
        "SCSI/SATA/USB"
    } else if name.starts_with("loop") {
        "Loop"
    } else if name.starts_with("ram") {
        "RAM"
    } else {
        "Block"
    }
}

fn format_bytes(bytes: u64) -> String {
    let units = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut idx = 0;
    while value >= 1024.0 && idx < units.len() - 1 {
        value /= 1024.0;
        idx += 1;
    }
    let compact = if idx == 0 {
        format!("{bytes} {}", units[idx])
    } else {
        format!("{value:.1} {}", units[idx])
    };
    match idx {
        0 => compact,
        1 => format!("{compact} [{} B]", format_grouped_u64(bytes)),
        2 => format!("{compact} [{} KiB]", format_grouped_u64(bytes / 1024)),
        _ => format!(
            "{compact} [{} MiB]",
            format_grouped_u64(bytes / (1024 * 1024))
        ),
    }
}

fn format_grouped_u64(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, ch) in digits.chars().rev().enumerate() {
        if index > 0 && index % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out.chars().rev().collect()
}

fn target_mode_choice(mode: &TargetMode) -> &'static str {
    match mode {
        TargetMode::RawDevice => "1",
        TargetMode::FileBased => "2",
    }
}

fn workload_choice(workload: &WorkloadType) -> &'static str {
    match workload {
        WorkloadType::RandRead => "1",
        WorkloadType::RandWrite => "2",
        WorkloadType::RandRw => "3",
        WorkloadType::Read => "4",
        WorkloadType::Write => "5",
    }
}

fn addressing_choice(mode: &AddressingMode) -> &'static str {
    match mode {
        AddressingMode::WholeSelectedRange => "1",
        AddressingMode::SectorRange => "2",
        AddressingMode::ByteRange => "3",
    }
}

fn stop_choice(exact: bool) -> &'static str {
    if exact {
        "2"
    } else {
        "1"
    }
}

fn durability_choice(mode: &DurabilityMode) -> &'static str {
    match mode {
        DurabilityMode::Performance => "1",
        DurabilityMode::BatchDurable => "2",
        DurabilityMode::StrictDurable => "3",
    }
}

fn default_profile_name(profile: &Profile) -> String {
    format!(
        "{}-{}",
        match profile.target.mode {
            TargetMode::RawDevice => "raw",
            TargetMode::FileBased => "file",
        },
        profile.workload.test_type
    )
}

fn parse_yes_no(value: &str) -> Result<bool> {
    match value.to_ascii_lowercase().as_str() {
        "y" | "yes" | "true" | "1" => Ok(true),
        "n" | "no" | "false" | "0" => Ok(false),
        _ => bail!("expected yes/no"),
    }
}

fn select_from_menu(
    title: &str,
    breadcrumb: &str,
    summary_rows: Vec<(String, String)>,
    columns: &[SelectorColumn<'_>],
    items: &[MenuItem],
    default: usize,
) -> Result<Option<usize>> {
    if items.is_empty() {
        return Ok(None);
    }
    if unsafe { libc::isatty(libc::STDIN_FILENO) } != 1
        || unsafe { libc::isatty(libc::STDOUT_FILENO) } != 1
    {
        bail!(
            "interactive selector requires a terminal; use a real TTY or run a direct command such as `emmc-lab doctor`, `emmc-lab run --profile ...`, or `emmc-lab report --session ...`"
        );
    }
    let _terminal = RawTerminalGuard::new()?;
    let _resize = ResizeSignalGuard::install();
    let mut selected = default.min(items.len().saturating_sub(1));
    loop {
        let rows = items
            .iter()
            .map(|item| SelectorRow {
                cells: item.columns.clone(),
                detail: item.detail.clone(),
            })
            .collect::<Vec<_>>();
        redraw_managed_screen(&render_selector_screen(
            title,
            breadcrumb,
            &summary_rows,
            columns,
            &rows,
            selected,
            "Up/Down or j/k move  Enter open  Esc back  q quit  Ctrl-C terminate",
        ))?;
        rewrite_selector_status(selector_status())?;
        match read_menu_key()? {
            MenuKey::Up => {
                selected = selected.saturating_sub(1);
            }
            MenuKey::Down => {
                selected = (selected + 1).min(items.len().saturating_sub(1));
            }
            MenuKey::Enter => return Ok(Some(selected)),
            MenuKey::Escape | MenuKey::Quit => return Ok(None),
            MenuKey::Resize => continue,
        }
    }
}

fn pause() -> Result<()> {
    println!("Press Enter to continue...");
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    Ok(())
}

fn host_total_bytes_read(device: &Path) -> Option<u64> {
    let name = device.file_name()?.to_string_lossy().to_string();
    let disk_name = canonical_diskstats_name(&name);
    let stats = diskstats_map();
    stats
        .get(&disk_name)
        .and_then(|values| values.get(2).copied())
        .map(|sectors| sectors.saturating_mul(512))
}

fn canonical_diskstats_name(name: &str) -> String {
    if let Some(index) = name.rfind('p') {
        if name[index + 1..].chars().all(|ch| ch.is_ascii_digit()) {
            return name[..index].to_string();
        }
    }
    name.to_string()
}

fn parse_lba_range(value: &str) -> Result<std::ops::Range<u64>> {
    let (start, end) = value
        .split_once('-')
        .ok_or_else(|| anyhow::anyhow!("invalid lba range {value}; expected START-END"))?;
    let start = start
        .trim()
        .parse::<u64>()
        .map_err(|_| anyhow::anyhow!("invalid lba start in {value}"))?;
    let end = end
        .trim()
        .parse::<u64>()
        .map_err(|_| anyhow::anyhow!("invalid lba end in {value}"))?;
    if end <= start {
        anyhow::bail!("invalid lba range {value}; end must be greater than start");
    }
    Ok(start..end)
}

impl Spinner {
    fn start(label: &str) -> Self {
        if unsafe { libc::isatty(libc::STDERR_FILENO) } != 1 {
            return Self {
                stop: Arc::new(AtomicBool::new(true)),
                handle: None,
            };
        }
        let stop = Arc::new(AtomicBool::new(false));
        let signal = Arc::clone(&stop);
        let label_text = label.to_string();
        let clear_width = label_text.chars().count() + 3;
        let handle = thread::spawn(move || {
            let frames = ['|', '/', '-', '\\'];
            let mut index = 0usize;
            while !signal.load(Ordering::Relaxed) {
                eprint!("\r{} {}", label_text, frames[index % frames.len()]);
                let _ = io::stderr().flush();
                index = index.wrapping_add(1);
                thread::sleep(Duration::from_millis(100));
            }
            eprint!("\r{blank}\r", blank = " ".repeat(clear_width));
            let _ = io::stderr().flush();
        });
        Self {
            stop,
            handle: Some(handle),
        }
    }

    fn finish(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for Spinner {
    fn drop(&mut self) {
        self.finish();
    }
}

fn with_spinner<T, F>(label: &str, op: F) -> T
where
    F: FnOnce() -> T,
{
    let spinner = Spinner::start(label);
    let result = panic::catch_unwind(AssertUnwindSafe(op));
    drop(spinner);
    match result {
        Ok(value) => value,
        Err(err) => panic::resume_unwind(err),
    }
}

struct RawTerminalGuard {
    original: libc::termios,
    active: bool,
}

impl RawTerminalGuard {
    fn new() -> Result<Self> {
        if unsafe { libc::isatty(libc::STDIN_FILENO) } != 1 {
            return Ok(Self {
                original: unsafe { std::mem::zeroed() },
                active: false,
            });
        }
        let mut original = MaybeUninit::<libc::termios>::uninit();
        if unsafe { libc::tcgetattr(libc::STDIN_FILENO, original.as_mut_ptr()) } != 0 {
            bail!("failed to read terminal mode");
        }
        let original = unsafe { original.assume_init() };
        let mut raw = original;
        raw.c_lflag &= !(libc::ICANON | libc::ECHO);
        raw.c_cc[libc::VMIN] = 0;
        raw.c_cc[libc::VTIME] = 0;
        if unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &raw) } != 0 {
            bail!("failed to set terminal raw mode");
        }
        print!("\u{1b}[?1049h\u{1b}[?25l\u{1b}[H\u{1b}[J");
        io::stdout().flush()?;
        Ok(Self {
            original,
            active: true,
        })
    }
}

impl Drop for RawTerminalGuard {
    fn drop(&mut self) {
        if self.active {
            let _ = write!(io::stdout(), "\u{1b}[?25h\u{1b}[?1049l");
            let _ = io::stdout().flush();
            unsafe {
                libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &self.original);
            }
        }
    }
}

fn redraw_live_monitor(report: &DiagnosticReport, interval_ms: u64) -> Result<()> {
    redraw_managed_screen(&render_live_monitor(report, interval_ms))
}

fn redraw_managed_screen(content: &str) -> Result<()> {
    write!(io::stdout(), "\u{1b}[H\u{1b}[J{}", content)?;
    io::stdout().flush()?;
    Ok(())
}

fn poll_keypress_until(timeout: Duration) -> Result<Option<u8>> {
    let timeout_ms = timeout.as_millis().min(i32::MAX as u128) as i32;
    let mut pollfd = libc::pollfd {
        fd: libc::STDIN_FILENO,
        events: libc::POLLIN,
        revents: 0,
    };
    let rc = unsafe { libc::poll(&mut pollfd, 1, timeout_ms) };
    if rc < 0 {
        let err = io::Error::last_os_error();
        if err.kind() == io::ErrorKind::Interrupted {
            return Ok(None);
        }
        bail!("failed to poll stdin");
    }
    if rc == 0 || pollfd.revents & libc::POLLIN == 0 {
        return Ok(None);
    }
    let mut byte = [0_u8; 1];
    let read = unsafe { libc::read(libc::STDIN_FILENO, byte.as_mut_ptr().cast(), 1) };
    if read == 1 {
        Ok(Some(byte[0]))
    } else {
        Ok(None)
    }
}

fn read_menu_key() -> Result<MenuKey> {
    loop {
        let Some(byte) = poll_keypress_until(Duration::from_secs(60))? else {
            if RESIZE_FLAG.swap(false, Ordering::Relaxed) {
                return Ok(MenuKey::Resize);
            }
            continue;
        };
        return match byte {
            b'\r' | b'\n' => Ok(MenuKey::Enter),
            b'k' | b'K' => Ok(MenuKey::Up),
            b'j' | b'J' => Ok(MenuKey::Down),
            b'q' | b'Q' | 3 => Ok(MenuKey::Quit),
            27 => parse_escape_sequence(),
            _ => continue,
        };
    }
}

fn selector_status() -> &'static str {
    "  Use Up/Down or j/k to move. Press Enter to open."
}

fn rewrite_selector_status(status: &str) -> Result<()> {
    write!(io::stdout(), "\r\u{1b}[K{}", status)?;
    io::stdout().flush()?;
    Ok(())
}

struct ResizeSignalGuard {
    previous: libc::sighandler_t,
}

impl ResizeSignalGuard {
    fn install() -> Self {
        RESIZE_FLAG.store(false, Ordering::Relaxed);
        let previous = unsafe {
            libc::signal(
                libc::SIGWINCH,
                handle_sigwinch as *const () as libc::sighandler_t,
            )
        };
        Self { previous }
    }
}

impl Drop for ResizeSignalGuard {
    fn drop(&mut self) {
        unsafe {
            libc::signal(libc::SIGWINCH, self.previous);
        }
    }
}

fn parse_escape_sequence() -> Result<MenuKey> {
    let Some(next) = poll_keypress_until(Duration::from_millis(150))? else {
        return Ok(MenuKey::Escape);
    };
    if next != b'[' {
        return Ok(MenuKey::Escape);
    }
    let Some(code) = poll_keypress_until(Duration::from_millis(150))? else {
        return Ok(MenuKey::Escape);
    };
    match code {
        b'A' => Ok(MenuKey::Up),
        b'B' => Ok(MenuKey::Down),
        _ => Ok(MenuKey::Escape),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        adjusted_block_size, block_size_floor, device_rank, format_bytes, format_grouped_u64,
        is_auxiliary_device, is_single_sector_target, normalize_block_size,
        normalize_file_target_path, parse_yes_no, PromptBuffer,
    };
    use crate::profile::{AddressingMode, Profile, TargetMode};
    use crate::system::DeviceInfo;
    use std::path::PathBuf;
    use tempfile::tempdir;

    #[test]
    fn parses_menu_booleans() {
        assert!(parse_yes_no("y").expect("yes"));
        assert!(!parse_yes_no("n").expect("no"));
    }

    #[test]
    fn ranks_mmc_before_loop_and_ram() {
        let mmc = DeviceInfo {
            name: "mmcblk0".to_string(),
            path: PathBuf::from("/dev/mmcblk0"),
            ..DeviceInfo::default()
        };
        let loop0 = DeviceInfo {
            name: "loop0".to_string(),
            path: PathBuf::from("/dev/loop0"),
            ..DeviceInfo::default()
        };
        let ram0 = DeviceInfo {
            name: "ram0".to_string(),
            path: PathBuf::from("/dev/ram0"),
            ..DeviceInfo::default()
        };
        assert!(device_rank(&mmc) < device_rank(&loop0));
        assert!(device_rank(&loop0) < device_rank(&ram0));
    }

    #[test]
    fn detects_auxiliary_devices() {
        let mmc = DeviceInfo {
            name: "mmcblk0".to_string(),
            path: PathBuf::from("/dev/mmcblk0"),
            ..DeviceInfo::default()
        };
        let loop0 = DeviceInfo {
            name: "loop0".to_string(),
            path: PathBuf::from("/dev/loop0"),
            ..DeviceInfo::default()
        };
        assert!(!is_auxiliary_device(&mmc));
        assert!(is_auxiliary_device(&loop0));
    }

    #[test]
    fn normalizes_small_direct_io_block_sizes() {
        let profile = Profile::default_named("test");
        assert_eq!(block_size_floor(&profile, 512), 4096);
        assert_eq!(normalize_block_size(12, 512, 4096), 4096);
        assert_eq!(normalize_block_size(4097, 512, 4096), 4608);
    }

    #[test]
    fn keeps_single_sector_block_size_at_logical_sector() {
        let mut profile = Profile::default_named("single-sector");
        profile.addressing.mode = AddressingMode::SectorRange;
        profile.addressing.start_sector = Some(1234);
        profile.addressing.sector_count = Some(1);
        assert!(is_single_sector_target(&profile));
        assert_eq!(block_size_floor(&profile, 512), 512);
        assert_eq!(normalize_block_size(12, 512, 512), 512);
    }

    #[test]
    fn formats_grouped_sizes_for_operator_prompts() {
        assert_eq!(format_grouped_u64(2048), "2,048");
        assert_eq!(format_bytes(2 * 1024 * 1024), "2.0 MiB [2,048 KiB]");
        assert_eq!(format_bytes(1024 * 1024 * 1024), "1.0 GiB [1,024 MiB]");
    }

    #[test]
    fn reduces_sector_range_block_size_to_preserve_start_alignment() {
        let mut profile = Profile::default_named("sector-range");
        profile.addressing.mode = AddressingMode::SectorRange;
        profile.addressing.start_sector = Some(4);
        profile.addressing.sector_count = Some(1_048_576);
        profile.workload.block_size_bytes = 4096;
        assert_eq!(adjusted_block_size(&profile, 512), 2048);
    }

    #[test]
    fn converts_directory_target_into_default_file() {
        let dir = tempdir().expect("tempdir");
        let normalized =
            normalize_file_target_path(&TargetMode::FileBased, dir.path().to_path_buf());
        assert_eq!(normalized, dir.path().join("emmc-lab.bin"));
    }

    #[test]
    fn prompt_buffer_edits_seeded_invalid_value() {
        let mut buffer = PromptBuffer::from_seed(Some("12x4"));
        buffer.move_left();
        buffer.move_left();
        buffer.backspace();
        buffer.insert('3');
        assert_eq!(buffer.text, "13x4");
        assert_eq!(buffer.cursor, 2);
    }

    #[test]
    fn prompt_buffer_delete_removes_character_under_cursor() {
        let mut buffer = PromptBuffer::from_seed(Some("1a24"));
        buffer.move_home();
        buffer.move_right();
        buffer.delete();
        assert_eq!(buffer.text, "124");
        assert_eq!(buffer.cursor, 1);
    }

    #[test]
    fn prompt_buffer_home_end_and_bounds_are_stable() {
        let mut buffer = PromptBuffer::from_seed(Some("512"));
        buffer.move_home();
        buffer.backspace();
        assert_eq!(buffer.text, "512");
        assert_eq!(buffer.cursor, 0);
        buffer.move_end();
        buffer.move_right();
        assert_eq!(buffer.cursor, 3);
        buffer.delete();
        assert_eq!(buffer.text, "512");
    }

    #[test]
    fn prompt_buffer_inserts_in_middle_after_arrow_navigation() {
        let mut buffer = PromptBuffer::from_seed(Some("104"));
        buffer.move_left();
        buffer.insert('2');
        assert_eq!(buffer.text, "1024");
        assert_eq!(buffer.cursor, 3);
    }
}
