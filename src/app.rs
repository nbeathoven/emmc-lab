use crate::diagnostics::{
    run_deep_trace, run_live_sampler, run_live_sampler_until, DiagnosticReport,
};
use crate::engine::execute_profile;
use crate::health::{collect_health, read_emmc_health};
use crate::profile::{AddressingMode, DurabilityMode, Profile, TargetMode, WorkloadType};
use crate::report::{export_session, terminal_summary, ExportFormat};
use crate::storage::{
    infer_profile_path, list_profiles, list_sessions, load_session, save_session, SessionRecord,
};
use crate::system::{
    assess_raw_target_safety, block_device_size, collect_capabilities, collect_system_snapshot,
    doctor, list_devices, logical_block_size, AppPaths, DeviceInfo,
};
use crate::ui::{render_device_list, render_doctor_report, render_health_report, render_settings};
use anyhow::{bail, Result};
use chrono::Utc;
use std::env;
use std::ffi::CString;
use std::fs;
use std::io::{self, Write};
use std::os::unix::ffi::OsStrExt;
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

pub fn run_menu(paths: &AppPaths) -> Result<()> {
    loop {
        println!();
        println!("emmc-lab");
        println!("1. New Test Run");
        println!("2. Run Saved Profile");
        println!("3. Diagnostic Mode - Live Sampler");
        println!("4. Diagnostic Mode - Deep Trace");
        println!("5. Device Health / eMMC Health");
        println!("6. Reports / Export");
        println!("7. Settings / Paths / Defaults");
        println!("8. Help");
        println!("9. Exit");
        match prompt_menu_default(
            "Select an option [1-9]",
            "1",
            &["1", "2", "3", "4", "5", "6", "7", "8", "9"],
        )? {
            WizardInput::Value(value) if value == "1" => {
                if let Some(outcome) = wizard(paths, None)? {
                    if outcome.run_now {
                        let _ = run_profile_session(paths, outcome.profile)?;
                    }
                }
            }
            WizardInput::Value(value) if value == "2" => run_saved_profile_flow(paths)?,
            WizardInput::Value(value) if value == "3" => live_sampler_flow(paths)?,
            WizardInput::Value(value) if value == "4" => deep_trace_flow(paths)?,
            WizardInput::Value(value) if value == "5" => health_flow(paths)?,
            WizardInput::Value(value) if value == "6" => reports_flow(paths)?,
            WizardInput::Value(value) if value == "7" => settings_flow(paths)?,
            WizardInput::Value(value) if value == "8" => print_help(paths),
            WizardInput::Value(value) if value == "9" => break,
            WizardInput::Back => continue,
            WizardInput::Cancel => break,
            WizardInput::Value(_) => unreachable!(),
        }
    }
    Ok(())
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
        record.health_before = Some(read_emmc_health(&profile.target.path)?);
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

    let (run_summary, intervals) = execute_profile(&profile)?;
    record.run_summary = Some(run_summary);
    record.interval_stats = intervals;

    if profile.telemetry.health_telemetry {
        record.health_after = Some(read_emmc_health(&profile.target.path)?);
    }

    if let Some(handle) = diag_handle {
        stop_signal.store(true, Ordering::Relaxed);
        if let Ok(Ok(diag)) = handle.join() {
            record.contamination_note = derive_interference_note(&diag);
            record.diagnostics = Some(diag);
        }
    }

    let path = save_session(paths, &record)?;
    println!("Saved session: {}", path.display());

    if profile.reporting.export_json {
        let _ = export_session(paths, &record.session_id, ExportFormat::Json);
    }
    if profile.reporting.export_csv {
        let _ = export_session(paths, &record.session_id, ExportFormat::Csv);
    }
    if profile.reporting.export_html {
        let _ = export_session(paths, &record.session_id, ExportFormat::Html);
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

pub fn run_health(paths: &AppPaths, device: &Path) -> Result<()> {
    let report = collect_health(paths, device)?;
    println!("{}", render_health_report(&report));
    Ok(())
}

pub fn run_diag_sample(
    paths: &AppPaths,
    duration_seconds: u64,
    interval_ms: u64,
) -> Result<String> {
    let session_id = session_id();
    let mut record = SessionRecord::new(session_id.clone());
    let report = run_live_sampler(
        Duration::from_secs(duration_seconds.max(1)),
        Duration::from_millis(interval_ms.max(100)),
        Some(session_id.clone()),
    )?;
    record.diagnostics = Some(report);
    record.notes.push(
        "Logical syscall bytes are reported separately from storage-layer bytes in procfs sampler mode."
            .to_string(),
    );
    let path = save_session(paths, &record)?;
    println!("Saved session: {}", path.display());
    println!("{}", terminal_summary(&record));
    Ok(session_id)
}

pub fn run_diag_trace(
    paths: &AppPaths,
    duration_seconds: u64,
    fallback_to_sampler: bool,
) -> Result<String> {
    let session_id = session_id();
    let mut record = SessionRecord::new(session_id.clone());
    let report = run_deep_trace(
        Duration::from_secs(duration_seconds.max(1)),
        Some(session_id.clone()),
        fallback_to_sampler,
    )?;
    record.diagnostics = Some(report);
    let path = save_session(paths, &record)?;
    println!("Saved session: {}", path.display());
    println!("{}", terminal_summary(&record));
    Ok(session_id)
}

pub fn run_report(paths: &AppPaths, session_id: &str) -> Result<()> {
    let record = load_session(paths, session_id)?;
    println!("{}", terminal_summary(&record));
    Ok(())
}

pub fn run_export(paths: &AppPaths, session_id: &str, format: ExportFormat) -> Result<()> {
    let files = export_session(paths, session_id, format)?;
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
    let report = doctor(paths);
    println!("{}", render_doctor_report(&report));
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
    while step < 15 {
        let hints = wizard_target_hints(&profile);
        match step {
            0 => {
                match prompt_menu_default(
                    "Test mode [1=raw device, 2=file-based]",
                    target_mode_choice(&profile.target.mode),
                    &["1", "2"],
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
            2 => {
                match prompt_menu_default(
                    "Test type [1=randread, 2=randwrite, 3=randrw, 4=read, 5=write]",
                    workload_choice(&profile.workload.test_type),
                    &["1", "2", "3", "4", "5"],
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
            1 => match prompt_target_path(&profile.target.mode)? {
                WizardInput::Value(path) => {
                    let normalized =
                        normalize_file_target_path(&profile.target.mode, PathBuf::from(path));
                    profile.target.path = normalized;
                    let hints = wizard_target_hints(&profile);
                    println!("Discovered target: {}", hints.target_label);
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
                                "Create file at runtime if missing bytes [recommended {}]",
                                format_bytes(default_size)
                            ),
                            default_size.clamp(min_size, max_size),
                            min_size,
                            max_size,
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
            3 => match prompt_menu_default(
                &format!(
                    "Addressing [1=whole range, 2=sector range, 3=byte range] [{}]",
                    range_hint_label(&hints)
                ),
                addressing_choice(&profile.addressing.mode),
                &["1", "2", "3"],
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
                    _ => unreachable!(),
                },
                WizardInput::Back => {
                    step -= 1;
                    continue;
                }
                WizardInput::Cancel => return Ok(None),
            },
            4 => match prompt_menu_default(
                "Stop condition [1=runtime seconds, 2=exact operation count]",
                stop_choice(profile.workload.exact_op_count.is_some()),
                &["1", "2"],
            )? {
                WizardInput::Value(value) => match value.as_str() {
                    "1" => {
                        let runtime = match prompt_u64_in_range(
                            "Runtime seconds",
                            profile.workload.runtime_seconds.unwrap_or(30),
                            5,
                            86_400,
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
                    "Block size bytes [recommended {}, range {}-1048576, sector {} B]",
                    suggested_block_size(&hints),
                    hints.logical_sector_size.max(512),
                    hints.logical_sector_size
                ),
                normalize_block_size(profile.workload.block_size_bytes, hints.logical_sector_size),
                hints.logical_sector_size.max(512) as usize,
                1_048_576,
            )? {
                WizardInput::Value(value) => profile.workload.block_size_bytes = value,
                WizardInput::Back => {
                    step -= 1;
                    continue;
                }
                WizardInput::Cancel => return Ok(None),
            },
            6 => match prompt_u32_in_range(
                "Queue depth [recommended 1-8]",
                profile.workload.queue_depth.clamp(1, 8),
                1,
                64,
            )? {
                WizardInput::Value(value) => profile.workload.queue_depth = value,
                WizardInput::Back => {
                    step -= 1;
                    continue;
                }
                WizardInput::Cancel => return Ok(None),
            },
            7 => match prompt_usize_in_range(
                &format!(
                    "Number of workers [recommended 1-{}]",
                    suggested_worker_count()
                ),
                profile
                    .workload
                    .worker_count
                    .max(1)
                    .min(suggested_worker_count()),
                1,
                64,
            )? {
                WizardInput::Value(value) => profile.workload.worker_count = value,
                WizardInput::Back => {
                    step -= 1;
                    continue;
                }
                WizardInput::Cancel => return Ok(None),
            },
            8 => {
                match prompt_u64_in_range("Random seed", profile.workload.random_seed, 1, u64::MAX)?
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
                    "Direct I/O [recommended {} for {}]",
                    if profile.target.mode == TargetMode::RawDevice {
                        "y"
                    } else {
                        "n"
                    },
                    profile.target.mode
                ),
                profile.workload.direct_io,
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
                    println!("Verification applies to write workloads only. Keeping it off.");
                } else {
                    match prompt_bool_default(
                        "Verification [recommended n unless validating writes]",
                        profile.verification.enabled,
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
                    println!("Read workloads do not need a durability mode. Using performance.");
                } else {
                    match prompt_menu_default(
                        "Durability [1=performance, 2=batch durable, 3=strict durable]",
                        durability_choice(&profile.durability.mode),
                        &["1", "2", "3"],
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
                    "Health telemetry [{}]",
                    if hints.health_supported {
                        "recommended y"
                    } else {
                        "recommended n for this target"
                    }
                ),
                profile.telemetry.health_telemetry && hints.health_supported,
            )? {
                WizardInput::Value(value) => profile.telemetry.health_telemetry = value,
                WizardInput::Back => {
                    step -= 1;
                    continue;
                }
                WizardInput::Cancel => return Ok(None),
            },
            13 => match prompt_bool_default(
                "Diagnostic capture during test [recommended n unless checking interference]",
                profile.diagnostics.capture_during_test,
            )? {
                WizardInput::Value(value) => profile.diagnostics.capture_during_test = value,
                WizardInput::Back => {
                    step -= 1;
                    continue;
                }
                WizardInput::Cancel => return Ok(None),
            },
            14 => match prompt_string_default("Save profile name", Some(&profile.name))? {
                WizardInput::Value(value) => profile.name = value,
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
        match prompt_menu_default("Choose action [1-5]", "1", &["1", "2", "3", "4", "5"])? {
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

fn run_saved_profile_flow(paths: &AppPaths) -> Result<()> {
    let profiles = list_profiles(paths)?;
    if profiles.is_empty() {
        println!(
            "No saved profiles found in {}",
            paths.profiles_dir.display()
        );
        return Ok(());
    }
    for (index, profile) in profiles.iter().enumerate() {
        println!("{}. {}", index + 1, profile.display());
    }
    let choice = match prompt_usize_in_range(
        &format!("Select profile number [0-{}]", profiles.len()),
        1,
        0,
        profiles.len(),
    )? {
        WizardInput::Value(value) => value,
        WizardInput::Back | WizardInput::Cancel => return Ok(()),
    };
    if choice == 0 || choice > profiles.len() {
        return Ok(());
    }
    let selected = &profiles[choice - 1];
    let profile = Profile::load(selected)?;
    println!("{}", profile.printable_summary());
    println!("1. Run");
    println!("2. Edit in wizard");
    println!("3. Back");
    match prompt_menu_default("Choose action [1-3]", "2", &["1", "2", "3"])? {
        WizardInput::Value(value) if value == "1" => {
            let _ = run_profile_session(paths, profile)?;
        }
        WizardInput::Value(value) if value == "2" => {
            if let Some(updated) = wizard(paths, Some(profile))? {
                if updated.run_now {
                    let _ = run_profile_session(paths, updated.profile)?;
                }
            }
        }
        _ => {}
    }
    Ok(())
}

fn live_sampler_flow(paths: &AppPaths) -> Result<()> {
    let duration =
        match prompt_u64_in_range("Sampler duration seconds [recommended 60]", 60, 1, 86_400)? {
            WizardInput::Value(value) => value,
            WizardInput::Back | WizardInput::Cancel => return Ok(()),
        };
    let interval = match prompt_u64_in_range(
        "Sample interval milliseconds [recommended 1000]",
        1000,
        100,
        60_000,
    )? {
        WizardInput::Value(value) => value,
        WizardInput::Back | WizardInput::Cancel => return Ok(()),
    };
    let _ = run_diag_sample(paths, duration, interval)?;
    Ok(())
}

fn deep_trace_flow(paths: &AppPaths) -> Result<()> {
    let duration =
        match prompt_u64_in_range("Deep trace duration seconds [recommended 30]", 30, 1, 3_600)? {
            WizardInput::Value(value) => value,
            WizardInput::Back | WizardInput::Cancel => return Ok(()),
        };
    let fallback =
        match prompt_bool_default("Fallback to sampler if deep trace unavailable?", true)? {
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
    run_health(paths, &target)
}

fn reports_flow(paths: &AppPaths) -> Result<()> {
    let sessions = list_sessions(paths)?;
    if sessions.is_empty() {
        println!("No sessions found");
        return Ok(());
    }
    for (index, session) in sessions.iter().enumerate() {
        println!("{}. {}", index + 1, session);
    }
    let choice = match prompt_usize_in_range(
        &format!("Select session number [0-{}]", sessions.len()),
        1,
        0,
        sessions.len(),
    )? {
        WizardInput::Value(value) => value,
        WizardInput::Back | WizardInput::Cancel => return Ok(()),
    };
    if choice == 0 || choice > sessions.len() {
        return Ok(());
    }
    let session_id = &sessions[choice - 1];
    println!("1. View terminal summary");
    println!("2. Export JSON");
    println!("3. Export CSV");
    println!("4. Export HTML");
    println!("5. Back");
    match prompt_menu_default("Choose action [1-5]", "1", &["1", "2", "3", "4", "5"])? {
        WizardInput::Value(value) if value == "1" => {
            run_report(paths, session_id)?;
            pause()?;
        }
        WizardInput::Value(value) if value == "2" => {
            run_export(paths, session_id, ExportFormat::Json)?;
            pause()?;
        }
        WizardInput::Value(value) if value == "3" => {
            run_export(paths, session_id, ExportFormat::Csv)?;
            pause()?;
        }
        WizardInput::Value(value) if value == "4" => {
            run_export(paths, session_id, ExportFormat::Html)?;
            pause()?;
        }
        _ => {}
    }
    Ok(())
}

fn settings_flow(paths: &AppPaths) -> Result<()> {
    let caps = collect_capabilities(paths);
    let system = collect_system_snapshot();
    println!("{}", render_settings(paths, &system, &caps));
    Ok(())
}

fn print_help(paths: &AppPaths) {
    println!("Interactive launch: emmc-lab");
    println!("Guided wizard: emmc-lab wizard");
    println!(
        "Run saved profile: emmc-lab run --profile {}",
        paths.profiles_dir.join("example.yaml").display()
    );
    println!("Exact 1,000,000 write run in selected logical sector range:");
    println!("  emmc-lab run --profile /path/to/million-writes.yaml --i-understand-this-will-destroy-data");
    println!("Live sampler for 60 seconds: emmc-lab diag sample --duration 60");
    println!("Deep trace for 30 seconds: emmc-lab diag trace --duration 30");
    println!("Run test + diagnostics together: configure diagnostic capture = on in the wizard or YAML profile");
    println!("Export HTML report: emmc-lab export --session <id> --format html");
    println!("Health check: emmc-lab health --device /dev/mmcblk0");
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
    loop {
        match default {
            Some(default) => print!("{} [{}]: ", label, default),
            None => print!("{}: ", label),
        }
        io::stdout().flush()?;
        let mut input = String::new();
        if io::stdin().read_line(&mut input)? == 0 {
            return Ok(WizardInput::Cancel);
        }
        let trimmed = input.trim();
        if trimmed.eq_ignore_ascii_case("back") {
            return Ok(WizardInput::Back);
        }
        if trimmed.eq_ignore_ascii_case("cancel") {
            return Ok(WizardInput::Cancel);
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

fn prompt_menu_default(
    label: &str,
    default: &str,
    allowed: &[&str],
) -> Result<WizardInput<String>> {
    loop {
        match prompt_string_default(label, Some(default))? {
            WizardInput::Value(value) if allowed.contains(&value.as_str()) => {
                return Ok(WizardInput::Value(value));
            }
            WizardInput::Value(_) => println!("Invalid choice"),
            WizardInput::Back => return Ok(WizardInput::Back),
            WizardInput::Cancel => return Ok(WizardInput::Cancel),
        }
    }
}

fn prompt_u64_in_range(label: &str, default: u64, min: u64, max: u64) -> Result<WizardInput<u64>> {
    let default_text = default.to_string();
    loop {
        let prompt_label = format!("{label} [{min}-{max}]");
        match prompt_string_default(&prompt_label, Some(&default_text))? {
            WizardInput::Value(value) => match value.parse::<u64>() {
                Ok(parsed) if parsed >= min && parsed <= max => {
                    return Ok(WizardInput::Value(parsed))
                }
                Ok(_) => println!("Value must be between {min} and {max}"),
                Err(err) => println!("Invalid value: {}", err),
            },
            WizardInput::Back => return Ok(WizardInput::Back),
            WizardInput::Cancel => return Ok(WizardInput::Cancel),
        }
    }
}

fn prompt_u32_in_range(label: &str, default: u32, min: u32, max: u32) -> Result<WizardInput<u32>> {
    match prompt_u64_in_range(label, default as u64, min as u64, max as u64)? {
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
) -> Result<WizardInput<usize>> {
    match prompt_u64_in_range(label, default as u64, min as u64, max as u64)? {
        WizardInput::Value(value) => Ok(WizardInput::Value(value as usize)),
        WizardInput::Back => Ok(WizardInput::Back),
        WizardInput::Cancel => Ok(WizardInput::Cancel),
    }
}

fn prompt_bool_default(label: &str, default: bool) -> Result<WizardInput<bool>> {
    let default_text = if default { "y" } else { "n" };
    loop {
        match prompt_string_default(&format!("{label} [y/n]"), Some(default_text))? {
            WizardInput::Value(value) => match parse_yes_no(&value) {
                Ok(parsed) => return Ok(WizardInput::Value(parsed)),
                Err(_) => println!("Expected y or n"),
            },
            WizardInput::Back => return Ok(WizardInput::Back),
            WizardInput::Cancel => return Ok(WizardInput::Cancel),
        }
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

    loop {
        println!("0. Back");
        for (index, option) in options.iter().enumerate() {
            let suffix = if index == 0 { " [default]" } else { "" };
            println!("{}. {}{}", index + 1, option.display(), suffix);
        }
        println!("{}. Enter path manually", options.len() + 1);
        match prompt_string_default(
            &format!("Select file target [0-{}]", options.len() + 1),
            Some("1"),
        )? {
            WizardInput::Value(value) => {
                let Ok(choice) = value.parse::<usize>() else {
                    println!("Invalid choice");
                    continue;
                };
                if choice == 0 {
                    return Ok(WizardInput::Back);
                }
                if choice <= options.len() {
                    return Ok(WizardInput::Value(
                        options[choice - 1].display().to_string(),
                    ));
                }
                if choice == options.len() + 1 {
                    match prompt_string_default(
                        "Enter file path",
                        Some(&options[0].display().to_string()),
                    )? {
                        WizardInput::Value(value) => return Ok(WizardInput::Value(value)),
                        WizardInput::Back => continue,
                        WizardInput::Cancel => return Ok(WizardInput::Cancel),
                    }
                }
                println!("Invalid choice");
            }
            WizardInput::Back => return Ok(WizardInput::Back),
            WizardInput::Cancel => return Ok(WizardInput::Cancel),
        }
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
        println!("0. Back");
        for (index, device) in visible_devices.iter().enumerate() {
            println!("{}. {}", index + 1, device_selection_label(device));
        }
        let mut next_index = visible_devices.len() + 1;
        let show_all_index = if !show_all && primary_devices.len() < devices.len() {
            println!("{}. Show all devices", next_index);
            let idx = next_index;
            next_index += 1;
            Some(idx)
        } else {
            None
        };
        let manual_index = next_index;
        if allow_manual {
            println!("{}. Enter path manually", manual_index);
        }
        match prompt_string_default(
            &format!(
                "{} [0-{}]",
                title,
                if allow_manual {
                    manual_index
                } else {
                    visible_devices.len()
                }
            ),
            Some("1"),
        )? {
            WizardInput::Value(value) => {
                let Ok(choice) = value.parse::<usize>() else {
                    println!("Invalid choice");
                    continue;
                };
                if choice == 0 {
                    return Ok(None);
                }
                if choice <= visible_devices.len() {
                    return Ok(Some(visible_devices[choice - 1].path.clone()));
                }
                if show_all_index == Some(choice) {
                    show_all = true;
                    continue;
                }
                if allow_manual && choice == manual_index {
                    match prompt_string_default("Enter device path", None)? {
                        WizardInput::Value(value) => return Ok(Some(PathBuf::from(value))),
                        WizardInput::Back => continue,
                        WizardInput::Cancel => return Ok(None),
                    }
                }
                println!("Invalid choice");
            }
            WizardInput::Back => return Ok(None),
            WizardInput::Cancel => return Ok(None),
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

fn default_target_path(mode: &TargetMode) -> PathBuf {
    match mode {
        TargetMode::RawDevice => PathBuf::from("/dev/mmcblk0"),
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
    if profile.workload.direct_io {
        let normalized =
            normalize_block_size(profile.workload.block_size_bytes, hints.logical_sector_size);
        if normalized != profile.workload.block_size_bytes {
            println!(
                "Adjusted block size from {} to {} for direct I/O alignment.",
                profile.workload.block_size_bytes, normalized
            );
            profile.workload.block_size_bytes = normalized;
        }
    }
    if !hints.health_supported {
        profile.telemetry.health_telemetry = false;
    }
    if profile.name.trim().is_empty() {
        profile.name = default_profile_name(profile);
    }
}

fn normalize_block_size(current: usize, logical_sector_size: u64) -> usize {
    let sector = logical_sector_size.max(512) as usize;
    let recommended = sector.max(4096);
    if current == 0 {
        return recommended;
    }
    if current < recommended {
        return recommended;
    }
    let rounded = ((current + sector - 1) / sector) * sector;
    rounded.max(recommended)
}

fn suggested_block_size(hints: &WizardTargetHints) -> usize {
    normalize_block_size(4096, hints.logical_sector_size)
}

fn suggested_worker_count() -> usize {
    std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1)
        .clamp(1, 8)
}

fn minimum_runtime_file_size(profile: &Profile, logical_sector_size: u64) -> u64 {
    let block = profile.workload.block_size_bytes.max(4096) as u64;
    let sector = logical_sector_size.max(512);
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

fn device_selection_label(device: &DeviceInfo) -> String {
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
        device
            .media_type
            .clone()
            .unwrap_or_else(|| device_family_name(&device.path).to_string()),
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
    if idx == 0 {
        format!("{bytes} {}", units[idx])
    } else {
        format!("{value:.1} {}", units[idx])
    }
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

fn pause() -> Result<()> {
    println!("Press Enter to continue...");
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        device_rank, is_auxiliary_device, normalize_block_size, normalize_file_target_path,
        parse_yes_no,
    };
    use crate::profile::TargetMode;
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
        assert_eq!(normalize_block_size(12, 512), 4096);
        assert_eq!(normalize_block_size(4097, 512), 4608);
    }

    #[test]
    fn converts_directory_target_into_default_file() {
        let dir = tempdir().expect("tempdir");
        let normalized =
            normalize_file_target_path(&TargetMode::FileBased, dir.path().to_path_buf());
        assert_eq!(normalized, dir.path().join("emmc-lab.bin"));
    }
}
