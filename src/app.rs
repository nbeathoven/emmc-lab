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
    assess_raw_target_safety, collect_capabilities, collect_system_snapshot, doctor, list_devices,
    AppPaths, DeviceInfo,
};
use crate::ui::{render_device_list, render_doctor_report, render_health_report, render_settings};
use anyhow::{bail, Result};
use chrono::Utc;
use std::env;
use std::io::{self, Write};
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
        match prompt("Select an option")?.as_str() {
            "1" => {
                if let Some(outcome) = wizard(paths, None)? {
                    if outcome.run_now {
                        let _ = run_profile_session(paths, outcome.profile)?;
                    }
                }
            }
            "2" => run_saved_profile_flow(paths)?,
            "3" => live_sampler_flow(paths)?,
            "4" => deep_trace_flow(paths)?,
            "5" => health_flow(paths)?,
            "6" => reports_flow(paths)?,
            "7" => settings_flow(paths)?,
            "8" => print_help(paths),
            "9" => break,
            _ => println!("Invalid choice"),
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
    while step < 15 {
        let value = match step {
            0 => prompt(
                "Test mode [1=raw device, 2=file-based] (type 'back' or 'cancel')",
            )?,
            1 => prompt_target_path(&profile.target.mode)?,
            2 => prompt("Test type [1=randread, 2=randwrite, 3=randrw, 4=read, 5=write]")?,
            3 => prompt(
                "Addressing [1=whole range, 2=start sector + sector count, 3=start offset bytes + range bytes]",
            )?,
            4 => prompt("Stop condition [1=runtime seconds, 2=exact operation count]")?,
            5 => prompt("Block size bytes")?,
            6 => prompt("Queue depth")?,
            7 => prompt("Number of workers")?,
            8 => prompt("Random seed")?,
            9 => prompt("Direct I/O [y/n]")?,
            10 => prompt("Verification [y/n]")?,
            11 => prompt("Durability [1=performance, 2=batch durable, 3=strict durable]")?,
            12 => prompt("Health telemetry [y/n]")?,
            13 => prompt("Diagnostic capture during test [y/n]")?,
            14 => prompt("Save profile name")?,
            _ => unreachable!(),
        };
        match value.as_str() {
            "back" => {
                if step > 0 {
                    step -= 1;
                }
                continue;
            }
            "cancel" => return Ok(None),
            _ => {}
        }

        match step {
            0 => {
                profile.target.mode = match value.as_str() {
                    "1" => TargetMode::RawDevice,
                    "2" => TargetMode::FileBased,
                    _ => {
                        println!("Invalid choice");
                        continue;
                    }
                }
            }
            1 => profile.target.path = PathBuf::from(value),
            2 => {
                profile.workload.test_type = match value.as_str() {
                    "1" => WorkloadType::RandRead,
                    "2" => WorkloadType::RandWrite,
                    "3" => WorkloadType::RandRw,
                    "4" => WorkloadType::Read,
                    "5" => WorkloadType::Write,
                    _ => {
                        println!("Invalid choice");
                        continue;
                    }
                }
            }
            3 => match value.as_str() {
                "1" => {
                    profile.addressing.mode = AddressingMode::WholeSelectedRange;
                    profile.addressing.start_sector = None;
                    profile.addressing.sector_count = None;
                    profile.addressing.start_offset_bytes = Some(0);
                    profile.addressing.range_size_bytes = None;
                }
                "2" => {
                    profile.addressing.mode = AddressingMode::SectorRange;
                    profile.addressing.start_sector = Some(prompt_parse::<u64>("Start sector")?);
                    profile.addressing.sector_count = Some(prompt_parse::<u64>("Sector count")?);
                    profile.addressing.start_offset_bytes = None;
                    profile.addressing.range_size_bytes = None;
                }
                "3" => {
                    profile.addressing.mode = AddressingMode::ByteRange;
                    profile.addressing.start_offset_bytes =
                        Some(prompt_parse::<u64>("Start offset bytes")?);
                    profile.addressing.range_size_bytes = Some(prompt_parse::<u64>("Range bytes")?);
                    profile.addressing.start_sector = None;
                    profile.addressing.sector_count = None;
                }
                _ => {
                    println!("Invalid choice");
                    continue;
                }
            },
            4 => match value.as_str() {
                "1" => {
                    profile.workload.runtime_seconds =
                        Some(prompt_parse::<u64>("Runtime seconds")?);
                    profile.workload.exact_op_count = None;
                }
                "2" => {
                    profile.workload.exact_op_count =
                        Some(prompt_parse::<u64>("Exact operation count")?);
                    profile.workload.runtime_seconds = None;
                }
                _ => {
                    println!("Invalid choice");
                    continue;
                }
            },
            5 => profile.workload.block_size_bytes = value.parse::<usize>()?,
            6 => profile.workload.queue_depth = value.parse::<u32>()?,
            7 => profile.workload.worker_count = value.parse::<usize>()?,
            8 => profile.workload.random_seed = value.parse::<u64>()?,
            9 => profile.workload.direct_io = parse_yes_no(&value)?,
            10 => {
                let enabled = parse_yes_no(&value)?;
                profile.workload.verify = enabled;
                profile.verification.enabled = enabled;
            }
            11 => {
                profile.durability.mode = match value.as_str() {
                    "1" => DurabilityMode::Performance,
                    "2" => DurabilityMode::BatchDurable,
                    "3" => DurabilityMode::StrictDurable,
                    _ => {
                        println!("Invalid choice");
                        continue;
                    }
                };
            }
            12 => profile.telemetry.health_telemetry = parse_yes_no(&value)?,
            13 => profile.diagnostics.capture_during_test = parse_yes_no(&value)?,
            14 => profile.name = value,
            _ => {}
        }
        step += 1;
    }

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
        match prompt("Choose action")?.as_str() {
            "1" => {
                profile.save(&profile_path)?;
                println!("Saved profile: {}", profile_path.display());
                return Ok(Some(WizardOutcome {
                    profile,
                    run_now: false,
                }));
            }
            "2" => {
                return Ok(Some(WizardOutcome {
                    profile,
                    run_now: true,
                }))
            }
            "3" => {
                profile.save(&profile_path)?;
                println!("Saved profile: {}", profile_path.display());
                return Ok(Some(WizardOutcome {
                    profile,
                    run_now: true,
                }));
            }
            "4" => return wizard(paths, Some(profile)),
            "5" | "cancel" => return Ok(None),
            _ => println!("Invalid choice"),
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
    let choice = prompt_parse::<usize>("Select profile number or 0 to go back")?;
    if choice == 0 || choice > profiles.len() {
        return Ok(());
    }
    let selected = &profiles[choice - 1];
    let profile = Profile::load(selected)?;
    println!("{}", profile.printable_summary());
    println!("1. Run");
    println!("2. Edit in wizard");
    println!("3. Back");
    match prompt("Choose action")?.as_str() {
        "1" => {
            let _ = run_profile_session(paths, profile)?;
        }
        "2" => {
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
    let duration = prompt_parse::<u64>("Sampler duration seconds")?;
    let interval = prompt_parse::<u64>("Sample interval milliseconds")?;
    let _ = run_diag_sample(paths, duration, interval)?;
    Ok(())
}

fn deep_trace_flow(paths: &AppPaths) -> Result<()> {
    let duration = prompt_parse::<u64>("Deep trace duration seconds")?;
    let fallback = parse_yes_no(&prompt(
        "Fallback to sampler if deep trace unavailable? [y/n]",
    )?)?;
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
    let choice = prompt_parse::<usize>("Select session number or 0 to go back")?;
    if choice == 0 || choice > sessions.len() {
        return Ok(());
    }
    let session_id = &sessions[choice - 1];
    println!("1. View terminal summary");
    println!("2. Export JSON");
    println!("3. Export CSV");
    println!("4. Export HTML");
    println!("5. Back");
    match prompt("Choose action")?.as_str() {
        "1" => {
            run_report(paths, session_id)?;
            pause()?;
        }
        "2" => {
            run_export(paths, session_id, ExportFormat::Json)?;
            pause()?;
        }
        "3" => {
            run_export(paths, session_id, ExportFormat::Csv)?;
            pause()?;
        }
        "4" => {
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

fn prompt(label: &str) -> Result<String> {
    loop {
        print!("{}: ", label);
        io::stdout().flush()?;
        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        let trimmed = input.trim().to_string();
        if !trimmed.is_empty() {
            return Ok(trimmed);
        }
        println!("Input cannot be empty");
    }
}

fn prompt_parse<T>(label: &str) -> Result<T>
where
    T: std::str::FromStr,
    <T as std::str::FromStr>::Err: std::fmt::Display,
{
    loop {
        let value = prompt(label)?;
        if value == "cancel" {
            bail!("cancelled");
        }
        match value.parse::<T>() {
            Ok(parsed) => return Ok(parsed),
            Err(err) => println!("Invalid value: {}", err),
        }
    }
}

fn prompt_target_path(mode: &TargetMode) -> Result<String> {
    match mode {
        TargetMode::RawDevice => {
            let devices = preferred_devices(list_devices()?);
            let selected = choose_device_path(&devices, "Select raw target device", true)?;
            match selected {
                Some(path) => Ok(path.display().to_string()),
                None => Ok("back".to_string()),
            }
        }
        TargetMode::FileBased => choose_file_target_path(),
    }
}

fn choose_file_target_path() -> Result<String> {
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
            println!("{}. {}", index + 1, option.display());
        }
        println!("{}. Enter path manually", options.len() + 1);
        let choice = prompt_parse::<usize>("Select file target")?;
        if choice == 0 {
            return Ok("back".to_string());
        }
        if choice <= options.len() {
            return Ok(options[choice - 1].display().to_string());
        }
        if choice == options.len() + 1 {
            return prompt("Enter file path");
        }
        println!("Invalid choice");
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
            println!(
                "{}. {} mounted={} root={} model={:?}",
                index + 1,
                device.path.display(),
                device.mounted,
                device.is_root_device,
                device.model
            );
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
        let choice = prompt_parse::<usize>(title)?;
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
            return Ok(Some(PathBuf::from(prompt("Enter device path")?)));
        }
        println!("Invalid choice");
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
    use super::{device_rank, is_auxiliary_device, parse_yes_no};
    use crate::system::DeviceInfo;
    use std::path::PathBuf;

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
}
