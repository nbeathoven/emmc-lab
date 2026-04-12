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
        Some(Commands::Health { device }) => app::run_health(&paths, &device),
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
