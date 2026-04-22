use crate::app;
use crate::report::ExportFormat;
use crate::system::AppPaths;
use crate::ui::{set_color_policy, ColorPolicy};
use anyhow::{anyhow, Result};
use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(
    name = "emmc-lab",
    version,
    about = "eMMC performance, endurance, health, and I/O diagnostics"
)]
struct Cli {
    #[arg(long, global = true, value_enum, default_value_t = ColorMode::Auto)]
    color: ColorMode,
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum ColorMode {
    Auto,
    Always,
    Never,
}

#[derive(Debug, Subcommand)]
enum Commands {
    Run {
        #[arg(long)]
        profile: PathBuf,
        #[arg(long = "i-understand-this-will-destroy-data", default_value_t = false)]
        i_understand_this_will_destroy_data: bool,
    },
    Wizard,
    Health {
        #[arg(long)]
        device: PathBuf,
        #[arg(long, default_value_t = false)]
        raw_extcsd: bool,
        #[arg(long)]
        page_size_kb: Option<u64>,
        #[arg(long)]
        pages_per_block: Option<u64>,
        #[arg(long, default_value_t = 100_000)]
        read_disturb_threshold: u64,
    },
    FileMap {
        #[arg()]
        file: Option<PathBuf>,
        #[arg(long)]
        dir: Option<PathBuf>,
        #[arg(long)]
        device: Option<PathBuf>,
        #[arg(long, default_value_t = false)]
        csv: bool,
        #[arg(long, default_value_t = false)]
        summary: bool,
        #[arg(long, default_value_t = false)]
        erase_align: bool,
    },
    LbaTrace {
        #[arg(long)]
        device: PathBuf,
        #[arg(long, default_value_t = 10)]
        duration: u64,
        #[arg(long)]
        lba_range: Option<String>,
        #[arg(long, value_delimiter = ',')]
        map_files: Vec<PathBuf>,
    },
    HealthCompare {
        #[arg(long)]
        device: PathBuf,
        #[arg(long)]
        fa_report: PathBuf,
        #[arg(long)]
        trace_report: Option<PathBuf>,
        #[arg(long)]
        html: Option<PathBuf>,
    },
    Discard {
        #[arg(long)]
        device: PathBuf,
        #[arg(long, default_value_t = false)]
        probe: bool,
        #[arg(long, default_value_t = false)]
        test: bool,
        #[arg(long)]
        offset: Option<u64>,
        #[arg(long)]
        length: Option<u64>,
        #[arg(long = "i-understand-this-will-discard-data", default_value_t = false)]
        i_understand_this_will_discard_data: bool,
    },
    Geometry {
        #[arg(long)]
        device: PathBuf,
    },
    Integrity {
        #[command(subcommand)]
        command: IntegrityCommand,
    },
    Diag {
        #[command(subcommand)]
        command: DiagCommand,
    },
    Report {
        #[arg(long)]
        session: String,
    },
    Export {
        #[arg(long)]
        session: String,
        #[arg(long)]
        format: String,
    },
    ListDevices,
    Doctor,
}

#[derive(Debug, Subcommand)]
enum DiagCommand {
    Monitor {
        #[arg(long, default_value_t = 0)]
        duration: u64,
        #[arg(long, default_value_t = 1000)]
        interval_ms: u64,
    },
    Sample {
        #[arg(long, default_value_t = 60)]
        duration: u64,
        #[arg(long, default_value_t = 1000)]
        interval_ms: u64,
    },
    Trace {
        #[arg(long, default_value_t = 30)]
        duration: u64,
        #[arg(long, default_value_t = true)]
        fallback_to_sampler: bool,
    },
}

#[derive(Debug, Subcommand)]
enum IntegrityCommand {
    Capture {
        #[arg(long)]
        device: PathBuf,
        #[arg(long)]
        algorithm: String,
        #[arg(long)]
        session: String,
    },
    Verify {
        #[arg(long)]
        device: PathBuf,
        #[arg(long)]
        session: String,
        #[arg(long)]
        note: String,
    },
    History {
        #[arg(long)]
        session: String,
    },
}

pub fn run() -> Result<()> {
    let cli = Cli::parse();
    set_color_policy(match cli.color {
        ColorMode::Auto => ColorPolicy::Auto,
        ColorMode::Always => ColorPolicy::Always,
        ColorMode::Never => ColorPolicy::Never,
    });
    let paths = AppPaths::discover()?;
    match cli.command {
        None => app::run_menu(&paths),
        Some(Commands::Run {
            profile,
            i_understand_this_will_destroy_data,
        }) => {
            let _ = app::run_profile_path(&paths, &profile, i_understand_this_will_destroy_data)?;
            Ok(())
        }
        Some(Commands::Wizard) => {
            if let Some(outcome) = app::wizard(&paths, None)? {
                if outcome.run_now {
                    let _ = app::run_profile_session(&paths, outcome.profile)?;
                }
            }
            Ok(())
        }
        Some(Commands::Health {
            device,
            raw_extcsd,
            page_size_kb,
            pages_per_block,
            read_disturb_threshold,
        }) => app::run_health(
            &paths,
            &device,
            raw_extcsd,
            page_size_kb,
            pages_per_block,
            read_disturb_threshold,
        ),
        Some(Commands::FileMap {
            file,
            dir,
            device,
            csv,
            summary,
            erase_align,
        }) => app::run_file_map(
            file.as_deref(),
            dir.as_deref(),
            device.as_deref(),
            csv,
            summary,
            erase_align,
        ),
        Some(Commands::LbaTrace {
            device,
            duration,
            lba_range,
            map_files,
        }) => app::run_lba_trace(&device, duration, lba_range.as_deref(), &map_files),
        Some(Commands::HealthCompare {
            device,
            fa_report,
            trace_report,
            html,
        }) => app::run_health_compare(
            &paths,
            &device,
            &fa_report,
            trace_report.as_deref(),
            html.as_deref(),
        ),
        Some(Commands::Discard {
            device,
            probe,
            test,
            offset,
            length,
            i_understand_this_will_discard_data,
        }) => app::run_discard(
            &device,
            probe,
            test,
            offset,
            length,
            i_understand_this_will_discard_data,
        ),
        Some(Commands::Geometry { device }) => app::run_geometry(&device),
        Some(Commands::Integrity { command }) => match command {
            IntegrityCommand::Capture {
                device,
                algorithm,
                session,
            } => app::run_integrity_capture(&paths, &session, &device, &algorithm),
            IntegrityCommand::Verify {
                device,
                session,
                note,
            } => app::run_integrity_verify(&paths, &session, &device, &note),
            IntegrityCommand::History { session } => app::run_integrity_history(&paths, &session),
        },
        Some(Commands::Diag { command }) => match command {
            DiagCommand::Monitor {
                duration,
                interval_ms,
            } => {
                app::run_diag_monitor(duration, interval_ms)?;
                Ok(())
            }
            DiagCommand::Sample {
                duration,
                interval_ms,
            } => {
                let _ = app::run_diag_sample(&paths, duration, interval_ms)?;
                Ok(())
            }
            DiagCommand::Trace {
                duration,
                fallback_to_sampler,
            } => {
                let _ = app::run_diag_trace(&paths, duration, fallback_to_sampler)?;
                Ok(())
            }
        },
        Some(Commands::Report { session }) => app::run_report(&paths, &session),
        Some(Commands::Export { session, format }) => {
            let format =
                ExportFormat::parse(&format).ok_or_else(|| anyhow!("unsupported format"))?;
            app::run_export(&paths, &session, format)
        }
        Some(Commands::ListDevices) => app::run_list_devices(),
        Some(Commands::Doctor) => app::run_doctor(&paths),
    }
}
