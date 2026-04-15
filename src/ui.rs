use crate::diagnostics::DiagnosticReport;
use crate::geometry::BlockGeometry;
use crate::health::{decode_life_time_estimate, decode_pre_eol_info, ExtCsdDecoded, HealthReport};
use crate::storage::SessionRecord;
use crate::system::{AppPaths, CapabilityReport, DeviceInfo, DoctorReport, SystemSnapshot};
use std::cmp::min;
use std::cmp::Reverse;
use std::env;
use std::ffi::CStr;
use std::fmt::Write as _;
use std::sync::atomic::{AtomicU8, Ordering};

const DEFAULT_TERMINAL_WIDTH: usize = 80;
const DEFAULT_TERMINAL_HEIGHT: usize = 40;
const ELLIPSIS: char = '…';
const ABBREVIATED_INLINE_LIMIT: usize = 25;

static COLOR_POLICY: AtomicU8 = AtomicU8::new(ColorPolicy::Auto as u8);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum ColorPolicy {
    Auto = 0,
    Always = 1,
    Never = 2,
}

#[derive(Clone, Copy)]
enum Align {
    Left,
    Right,
}

#[derive(Clone, Copy)]
struct Column<'a> {
    header: &'a str,
    min: usize,
    max: usize,
    align: Align,
}

#[derive(Clone)]
struct Ui {
    width: usize,
    height: usize,
    color: bool,
    host: String,
    runtime_env: String,
}

pub(crate) struct SelectorColumn<'a> {
    pub(crate) header: &'a str,
    pub(crate) min: usize,
    pub(crate) max: usize,
    pub(crate) align_right: bool,
}

pub(crate) struct SelectorRow {
    pub(crate) cells: Vec<String>,
    pub(crate) detail: String,
}

impl Ui {
    fn detect() -> Self {
        let width = stdout_width()
            .or_else(|| {
                env::var("COLUMNS")
                    .ok()
                    .and_then(|value| value.parse::<usize>().ok())
            })
            .unwrap_or(DEFAULT_TERMINAL_WIDTH)
            .clamp(24, 160);
        let height = stdout_height()
            .or_else(|| {
                env::var("LINES")
                    .ok()
                    .and_then(|value| value.parse::<usize>().ok())
            })
            .unwrap_or(DEFAULT_TERMINAL_HEIGHT)
            .clamp(20, 80);
        let no_color = env::var_os("NO_COLOR").is_some();
        let term = env::var("TERM").unwrap_or_default();
        let stdout_tty = unsafe { libc::isatty(libc::STDOUT_FILENO) == 1 };
        let color = resolve_color_enabled(current_color_policy(), no_color, &term, stdout_tty);
        let host = detect_hostname();
        let runtime_env =
            if env::var_os("SSH_CONNECTION").is_some() || env::var_os("SSH_TTY").is_some() {
                "ssh"
            } else {
                "local"
            }
            .to_string();
        Self {
            width,
            height,
            color,
            host,
            runtime_env,
        }
    }

    fn banner(&self, title: &str) -> String {
        let mut out = String::new();
        let utility = format!("emmc-lab v{}", env!("CARGO_PKG_VERSION"));
        let header = format!(
            "App {}  Host {}  Env {}  Term {}x{}",
            utility, self.host, self.runtime_env, self.width, self.height
        );
        let screen = format!("Screen {}", sanitize_inline(title));
        let line_width = visible_width(&header).max(visible_width(&screen)).max(24);
        let line = "=".repeat(min(self.width, line_width));
        let _ = writeln!(&mut out, "{}", self.paint(&header, "1"));
        let _ = writeln!(&mut out, "{}", self.paint(&screen, "1;36"));
        let _ = writeln!(&mut out, "{}", self.paint(&line, "36"));
        out
    }

    fn section(&self, title: &str) -> String {
        let mut out = String::new();
        let line = "-".repeat(min(self.width, title.len().max(8)));
        let _ = writeln!(&mut out);
        let _ = writeln!(&mut out, "{}", self.paint(title, "1"));
        let _ = writeln!(&mut out, "{}", self.paint(&line, "2"));
        out
    }

    fn note(&self, label: &str, value: &str) -> String {
        format!("{} {}\n", self.paint(label, "1;33"), sanitize_inline(value))
    }

    fn details_panel(&self, title: &str, body: &str) -> String {
        let mut out = self.section(title);
        let _ = writeln!(&mut out, "{}", body.trim());
        out
    }

    fn footer(&self, keys: &str) -> String {
        let mut out = self.section("Footer");
        let _ = writeln!(&mut out, "{}", sanitize_inline(keys));
        out
    }

    fn operator_context(&self, path: &str, scope: &str, risk: &str, enter: &str) -> String {
        self.kv_table(
            "Context",
            &[
                ("Path".to_string(), sanitize_inline(path)),
                ("Scope".to_string(), sanitize_inline(scope)),
                ("Risk".to_string(), sanitize_inline(risk)),
                ("Enter".to_string(), sanitize_inline(enter)),
            ],
            None,
        )
    }

    fn flow(&self, sections: &[String]) -> String {
        sections
            .iter()
            .filter(|section| !section.is_empty())
            .cloned()
            .collect::<Vec<_>>()
            .join("")
    }

    fn pair<F, G>(&self, left: F, right: G) -> String
    where
        F: Fn(&Ui) -> String,
        G: Fn(&Ui) -> String,
    {
        let half = (self.width.saturating_sub(2)) / 2;
        let constrained = self.constrained(half);
        let left_half = left(&constrained);
        let right_half = right(&constrained);
        if let Some(joined) = self.side_by_side(&left_half, &right_half) {
            return joined;
        }

        self.flow(&[left(self), right(self)])
    }

    fn constrained(&self, width: usize) -> Self {
        let mut constrained = self.clone();
        constrained.width = constrained.width.min(width.max(1));
        constrained
    }

    fn kv_table(&self, title: &str, rows: &[(String, String)], max_width: Option<usize>) -> String {
        let key_width = rows
            .iter()
            .map(|(key, _)| key.chars().count())
            .max()
            .unwrap_or(8)
            .clamp(8, 28);
        let value_width = self.width.saturating_sub(key_width + 7).max(16);
        let columns = [
            Column {
                header: "Field",
                min: 8,
                max: key_width,
                align: Align::Left,
            },
            Column {
                header: "Value",
                min: 16,
                max: value_width,
                align: Align::Left,
            },
        ];
        let table_rows = rows
            .iter()
            .map(|(key, value)| vec![key.clone(), value.clone()])
            .collect::<Vec<_>>();
        self.table(title, &columns, &table_rows, max_width)
    }

    fn table(
        &self,
        title: &str,
        columns: &[Column<'_>],
        rows: &[Vec<String>],
        max_width: Option<usize>,
    ) -> String {
        let mut out = self.section(title);
        if columns.is_empty() {
            return out;
        }

        let mut widths = columns
            .iter()
            .enumerate()
            .map(|(index, column)| {
                let data_width = rows
                    .iter()
                    .filter_map(|row| row.get(index))
                    .map(|value| value.chars().count())
                    .max()
                    .unwrap_or(0);
                column
                    .header
                    .chars()
                    .count()
                    .max(data_width)
                    .clamp(column.min, column.max)
            })
            .collect::<Vec<_>>();

        self.fit_width(
            columns,
            &mut widths,
            max_width.unwrap_or(self.width).min(self.width),
        );
        let border = self.border(&widths);
        let _ = writeln!(&mut out, "{}", border);
        let header = columns
            .iter()
            .enumerate()
            .map(|(index, column)| self.cell(column.header, widths[index], column.align))
            .collect::<Vec<_>>();
        let _ = writeln!(
            &mut out,
            "|{}|",
            header
                .into_iter()
                .map(|cell| self.paint(&cell, "1"))
                .collect::<Vec<_>>()
                .join("|")
        );
        let _ = writeln!(&mut out, "{}", border);
        for row in rows {
            let rendered = columns
                .iter()
                .enumerate()
                .map(|(index, column)| {
                    self.cell(
                        row.get(index).map(String::as_str).unwrap_or("-"),
                        widths[index],
                        column.align,
                    )
                })
                .collect::<Vec<_>>();
            let _ = writeln!(&mut out, "|{}|", rendered.join("|"));
        }
        let _ = writeln!(&mut out, "{}", border);
        out
    }

    fn fit_width(&self, columns: &[Column<'_>], widths: &mut [usize], max_width: usize) {
        if widths.is_empty() {
            return;
        }
        let budget = max_width
            .saturating_sub((widths.len() * 3).saturating_add(1))
            .max(widths.len());
        let desired = widths.to_vec();
        let minimum = columns
            .iter()
            .map(|column| column.min.max(1))
            .collect::<Vec<_>>();
        shrink_widths_to_budget(widths, &desired, &minimum, budget);
    }

    fn border(&self, widths: &[usize]) -> String {
        let mut out = String::new();
        out.push('+');
        for width in widths {
            out.push_str(&"-".repeat(*width + 2));
            out.push('+');
        }
        out
    }

    fn cell(&self, value: &str, width: usize, align: Align) -> String {
        let sanitized = prepare_inline_display(value);
        let display = if sanitized.is_empty() {
            "-"
        } else {
            &sanitized
        };
        let truncated = truncate_text(display, width);
        let pad = width.saturating_sub(truncated.chars().count());
        match align {
            Align::Left => format!(" {}{} ", truncated, " ".repeat(pad)),
            Align::Right => format!(" {}{} ", " ".repeat(pad), truncated),
        }
    }

    fn paint(&self, value: &str, code: &str) -> String {
        if self.color {
            format!("\u{1b}[{}m{}\u{1b}[0m", code, value)
        } else {
            value.to_string()
        }
    }

    fn side_by_side(&self, left: &str, right: &str) -> Option<String> {
        let gap = 2;
        let left_lines = left.lines().map(str::to_string).collect::<Vec<_>>();
        let right_lines = right.lines().map(str::to_string).collect::<Vec<_>>();
        let left_width = left_lines
            .iter()
            .map(|line| visible_width(line))
            .max()
            .unwrap_or(0);
        let right_width = right_lines
            .iter()
            .map(|line| visible_width(line))
            .max()
            .unwrap_or(0);
        if left_width == 0 || right_width == 0 || left_width + gap + right_width > self.width {
            return None;
        }

        let line_count = left_lines.len().max(right_lines.len());
        let mut out = String::new();
        for index in 0..line_count {
            let left_line = left_lines.get(index).map(String::as_str).unwrap_or("");
            let right_line = right_lines.get(index).map(String::as_str).unwrap_or("");
            let left_pad = left_width.saturating_sub(visible_width(left_line));
            let _ = writeln!(
                &mut out,
                "{}{}{}",
                left_line,
                " ".repeat(left_pad + gap),
                right_line
            );
        }
        Some(out)
    }
}

pub(crate) fn render_session_summary(record: &SessionRecord) -> String {
    let ui = Ui::detect();
    let mut out = ui.banner("EMMC-LAB SESSION SUMMARY");
    out.push_str(&ui.pair(
        |ui| {
            ui.operator_context(
                "Reports / Session Summary",
                "saved session review",
                "read-only",
                "not used on this screen",
            )
        },
        |ui| {
            ui.details_panel(
                "Details",
                "Overview metadata is shown first, then performance, reliability, diagnostics, and notes.",
            )
        },
    ));
    let mode = if record.run_summary.is_some() {
        "test-run"
    } else if record.diagnostics.is_some() {
        "diagnostic"
    } else {
        "session"
    };
    let mut session_rows = vec![
        ("Session ID".to_string(), record.session_id.clone()),
        ("Created".to_string(), record.created_at.to_rfc3339()),
        ("Mode".to_string(), mode.to_string()),
        (
            "Hostname".to_string(),
            opt_text(record.system.hostname.as_deref()),
        ),
        (
            "Kernel".to_string(),
            opt_text(record.system.kernel_version.as_deref()),
        ),
    ];
    if let Some(profile) = &record.profile {
        session_rows.push(("Profile".to_string(), profile.name.clone()));
    }
    let session_panel = ui.kv_table("Session", &session_rows, None);

    if let Some(run) = &record.run_summary {
        let run_rows = vec![
            ("Target".to_string(), run.target_path.display().to_string()),
            ("Target Mode".to_string(), run.target_mode.clone()),
            ("Backend".to_string(), run.backend.clone()),
            ("Started".to_string(), run.started_at.to_rfc3339()),
            ("Ended".to_string(), run.ended_at.to_rfc3339()),
            (
                "Logical Range".to_string(),
                format!(
                    "start={} bytes, length={} bytes, sector={} bytes",
                    run.addressed_range_start_bytes,
                    run.addressed_range_length_bytes,
                    run.logical_sector_size
                ),
            ),
        ];
        out.push_str(&ui.pair(
            |ui| ui.kv_table("Session", &session_rows, Some(ui.width)),
            |ui| ui.kv_table("Run Metadata", &run_rows, Some(ui.width)),
        ));

        let run_table = vec![
            vec![
                "Completed ops".to_string(),
                run.read_ios_completed.to_string(),
                run.write_ios_completed.to_string(),
            ],
            vec![
                "Completed data".to_string(),
                fmt_bytes(run.read_bytes_completed),
                fmt_bytes(run.write_bytes_completed),
            ],
            vec![
                "Failed ops".to_string(),
                run.failed_reads.to_string(),
                run.failed_writes.to_string(),
            ],
            vec![
                "Partial ops".to_string(),
                run.partial_reads.to_string(),
                run.partial_writes.to_string(),
            ],
            vec![
                "Latency p50".to_string(),
                fmt_us(run.read_latency.p50_us),
                fmt_us(run.write_latency.p50_us),
            ],
            vec![
                "Latency p95".to_string(),
                fmt_us(run.read_latency.p95_us),
                fmt_us(run.write_latency.p95_us),
            ],
            vec![
                "Latency p99".to_string(),
                fmt_us(run.read_latency.p99_us),
                fmt_us(run.write_latency.p99_us),
            ],
            vec![
                "Latency p99.9".to_string(),
                fmt_us(run.read_latency.p999_us),
                fmt_us(run.write_latency.p999_us),
            ],
        ];
        let reliability_rows = vec![
            ("Retries".to_string(), run.retries.to_string()),
            (
                "Verify Failures".to_string(),
                run.verify_failures.to_string(),
            ),
            ("Sync Failures".to_string(), run.sync_failures.to_string()),
            (
                "Verify Mismatches".to_string(),
                run.verify.checksum_mismatch.to_string(),
            ),
            ("Stale Data".to_string(), run.verify.stale_data.to_string()),
            (
                "Unexpected EOF".to_string(),
                run.verify.unexpected_eof.to_string(),
            ),
        ];
        out.push_str(&ui.pair(
            |ui| {
                ui.table(
                    "Run Performance",
                    &[
                        Column {
                            header: "Metric",
                            min: 14,
                            max: 18,
                            align: Align::Left,
                        },
                        Column {
                            header: "Reads",
                            min: 10,
                            max: 18,
                            align: Align::Right,
                        },
                        Column {
                            header: "Writes",
                            min: 10,
                            max: 18,
                            align: Align::Right,
                        },
                    ],
                    &run_table,
                    Some(ui.width),
                )
            },
            |ui| ui.kv_table("Reliability", &reliability_rows, Some(ui.width)),
        ));
        if let Some(first_failure) = &run.verify.first_failure {
            out.push_str(&ui.note(
                "Verify failure:",
                &format!(
                    "kind={} offset={} expected_crc={} actual_crc={} detail={}",
                    first_failure.kind,
                    first_failure.offset,
                    first_failure.expected_checksum,
                    first_failure.actual_checksum,
                    first_failure.detail
                ),
            ));
        }
    } else if let Some(diag) = &record.diagnostics {
        let diag_rows = build_diag_rows(diag);
        out.push_str(&ui.pair(
            |ui| ui.kv_table("Session", &session_rows, Some(ui.width)),
            |ui| ui.kv_table("Diagnostics", &diag_rows, Some(ui.width)),
        ));
    } else {
        out.push_str(&session_panel);
    }

    if let Some(diag) = &record.diagnostics {
        if record.run_summary.is_some() {
            let diag_rows = build_diag_rows(diag);
            out.push_str(&ui.kv_table("Diagnostics", &diag_rows, None));
        }
        out.push_str(&ui.note(
            "Attribution:",
            "Logical syscall bytes and storage-layer bytes are reported separately.",
        ));
        out.push_str(&ui.note("Detail:", &diag.attribution_note));
        if let Some(message) = &diag.fallback_message {
            out.push_str(&ui.note("Fallback:", message));
        }
        if let Some(backend) = &diag.trace_backend {
            out.push_str(&ui.note(
                "Trace backend:",
                &format!(
                    "{} (events={})",
                    backend,
                    diag.trace_event_count.unwrap_or(0)
                ),
            ));
        }

        let process_limit = row_limit(ui.height);
        let process_rows = diag
            .top_processes
            .iter()
            .take(process_limit)
            .map(|process| {
                vec![
                    process.pid.to_string(),
                    process.user.clone(),
                    process.command.clone(),
                    fmt_rate(process.logical_read_bytes_per_sec),
                    fmt_rate(process.logical_write_bytes_per_sec),
                    fmt_rate(process.storage_read_bytes_per_sec),
                    fmt_rate(process.storage_write_bytes_per_sec),
                    fmt_percent(process.observed_share_percent),
                ]
            })
            .collect::<Vec<_>>();
        if !process_rows.is_empty() {
            let path_rows = diag
                .top_processes
                .iter()
                .take(process_limit)
                .map(|process| {
                    vec![
                        process.pid.to_string(),
                        process.exe_path.clone(),
                        process.cwd.clone(),
                        process.hottest_file.clone(),
                        process.hottest_directory.clone(),
                    ]
                })
                .collect::<Vec<_>>();
            out.push_str(&ui.pair(
                |ui| {
                    ui.table(
                        "Top Processes",
                        &[
                            Column {
                                header: "PID",
                                min: 5,
                                max: 7,
                                align: Align::Right,
                            },
                            Column {
                                header: "User",
                                min: 6,
                                max: 12,
                                align: Align::Left,
                            },
                            Column {
                                header: "Command",
                                min: 10,
                                max: 18,
                                align: Align::Left,
                            },
                            Column {
                                header: "Log R/s",
                                min: 10,
                                max: 12,
                                align: Align::Right,
                            },
                            Column {
                                header: "Log W/s",
                                min: 10,
                                max: 12,
                                align: Align::Right,
                            },
                            Column {
                                header: "Store R/s",
                                min: 10,
                                max: 12,
                                align: Align::Right,
                            },
                            Column {
                                header: "Store W/s",
                                min: 10,
                                max: 12,
                                align: Align::Right,
                            },
                            Column {
                                header: "Share",
                                min: 7,
                                max: 8,
                                align: Align::Right,
                            },
                        ],
                        &process_rows,
                        Some(ui.width),
                    )
                },
                |ui| {
                    ui.table(
                        "Process Paths",
                        &[
                            Column {
                                header: "PID",
                                min: 5,
                                max: 7,
                                align: Align::Right,
                            },
                            Column {
                                header: "Executable",
                                min: 14,
                                max: 28,
                                align: Align::Left,
                            },
                            Column {
                                header: "CWD",
                                min: 14,
                                max: 24,
                                align: Align::Left,
                            },
                            Column {
                                header: "Hottest File",
                                min: 14,
                                max: 28,
                                align: Align::Left,
                            },
                            Column {
                                header: "Hottest Dir",
                                min: 14,
                                max: 24,
                                align: Align::Left,
                            },
                        ],
                        &path_rows,
                        Some(ui.width),
                    )
                },
            ));
        }

        let file_rows = diag
            .top_files
            .iter()
            .take(row_limit(ui.height))
            .map(|file| {
                vec![
                    file.file_path.clone(),
                    file.last_pid_touching_file.to_string(),
                    file.readers_count.to_string(),
                    file.writers_count.to_string(),
                    fmt_bytes(file.read_bytes),
                    fmt_bytes(file.write_bytes),
                    file.last_seen_offset_bytes
                        .map(fmt_bytes)
                        .unwrap_or_else(|| "-".to_string()),
                    file.last_seen_open_flags.clone(),
                ]
            })
            .collect::<Vec<_>>();
        if !file_rows.is_empty() {
            out.push_str(&ui.table(
                "Top Files",
                &[
                    Column {
                        header: "File",
                        min: 16,
                        max: 32,
                        align: Align::Left,
                    },
                    Column {
                        header: "PID",
                        min: 5,
                        max: 7,
                        align: Align::Right,
                    },
                    Column {
                        header: "Readers",
                        min: 7,
                        max: 8,
                        align: Align::Right,
                    },
                    Column {
                        header: "Writers",
                        min: 7,
                        max: 8,
                        align: Align::Right,
                    },
                    Column {
                        header: "Read Bytes",
                        min: 10,
                        max: 11,
                        align: Align::Right,
                    },
                    Column {
                        header: "Write Bytes",
                        min: 10,
                        max: 11,
                        align: Align::Right,
                    },
                    Column {
                        header: "FD Pos",
                        min: 8,
                        max: 11,
                        align: Align::Right,
                    },
                    Column {
                        header: "Flags",
                        min: 8,
                        max: 16,
                        align: Align::Left,
                    },
                ],
                &file_rows,
                None,
            ));
        }

        let directory_rows = diag
            .top_directories
            .iter()
            .take(row_limit(ui.height))
            .map(|dir| {
                vec![
                    dir.directory_path.clone(),
                    fmt_bytes(dir.total_read_bytes),
                    fmt_bytes(dir.total_write_bytes),
                    dir.file_count_touched.to_string(),
                    dir.top_processes
                        .iter()
                        .map(i32::to_string)
                        .collect::<Vec<_>>()
                        .join(","),
                ]
            })
            .collect::<Vec<_>>();
        if !directory_rows.is_empty() {
            out.push_str(&ui.table(
                "Top Directories",
                &[
                    Column {
                        header: "Directory",
                        min: 16,
                        max: 34,
                        align: Align::Left,
                    },
                    Column {
                        header: "Read Bytes",
                        min: 10,
                        max: 11,
                        align: Align::Right,
                    },
                    Column {
                        header: "Write Bytes",
                        min: 10,
                        max: 11,
                        align: Align::Right,
                    },
                    Column {
                        header: "Files",
                        min: 5,
                        max: 6,
                        align: Align::Right,
                    },
                    Column {
                        header: "Top PIDs",
                        min: 8,
                        max: 16,
                        align: Align::Left,
                    },
                ],
                &directory_rows,
                None,
            ));
        }

        let mut sorted_devices = diag.device_totals.clone();
        sorted_devices.sort_by_key(|device| diagnostic_device_rank(&device.device, device));
        let device_rows = sorted_devices
            .iter()
            .take(row_limit(ui.height))
            .map(|device| {
                vec![
                    device.device.clone(),
                    device.reads_completed.to_string(),
                    device.writes_completed.to_string(),
                    device.sectors_read.to_string(),
                    device.sectors_written.to_string(),
                    device.current_ios_in_progress.to_string(),
                ]
            })
            .collect::<Vec<_>>();
        if !device_rows.is_empty() {
            out.push_str(&ui.table(
                "Device Totals",
                &[
                    Column {
                        header: "Device",
                        min: 10,
                        max: 14,
                        align: Align::Left,
                    },
                    Column {
                        header: "Reads",
                        min: 7,
                        max: 9,
                        align: Align::Right,
                    },
                    Column {
                        header: "Writes",
                        min: 7,
                        max: 9,
                        align: Align::Right,
                    },
                    Column {
                        header: "Sec Read",
                        min: 8,
                        max: 10,
                        align: Align::Right,
                    },
                    Column {
                        header: "Sec Write",
                        min: 9,
                        max: 10,
                        align: Align::Right,
                    },
                    Column {
                        header: "InFlight",
                        min: 8,
                        max: 9,
                        align: Align::Right,
                    },
                ],
                &device_rows,
                None,
            ));
        }
    }

    if let Some(discard) = &record.discard_capability {
        out.push_str(&ui.kv_table("Discard Capability", &discard_rows(discard), None));
    }

    if let Some(before) = &record.health_before {
        let mut health_rows = vec![(
            "Before".to_string(),
            format!(
                "available={} pre_eol={} life_a={} life_b={}",
                before.available,
                opt_text(before.pre_eol_info.as_deref()),
                opt_text(before.device_life_time_est_typ_a.as_deref()),
                opt_text(before.device_life_time_est_typ_b.as_deref())
            ),
        )];
        if let Some(after) = &record.health_after {
            health_rows.push((
                "After".to_string(),
                format!(
                    "available={} pre_eol={} life_a={} life_b={}",
                    after.available,
                    opt_text(after.pre_eol_info.as_deref()),
                    opt_text(after.device_life_time_est_typ_a.as_deref()),
                    opt_text(after.device_life_time_est_typ_b.as_deref())
                ),
            ));
        }
        out.push_str(&ui.kv_table("Health Snapshots", &health_rows, None));
    }

    if let Some(note) = &record.contamination_note {
        out.push_str(&ui.note("Interference:", note));
    }
    if !record.notes.is_empty() {
        out.push_str(&ui.section("Notes"));
        for note in &record.notes {
            let _ = writeln!(&mut out, "- {}", note);
        }
    }
    out
}

pub(crate) fn render_selector_screen(
    title: &str,
    breadcrumb: &str,
    summary_rows: &[(String, String)],
    columns: &[SelectorColumn<'_>],
    rows: &[SelectorRow],
    selected: usize,
    footer: &str,
) -> String {
    let ui = Ui::detect();
    let mut out = ui.banner(title);
    let scope = if title.contains("REPORT") {
        "saved session selection"
    } else if title.contains("WORKLOAD") {
        "workload selection"
    } else if title.contains("DIAGNOSTIC") {
        "diagnostic selection"
    } else {
        "menu selection"
    };
    let row_cells = rows
        .iter()
        .enumerate()
        .map(|(index, row)| {
            let marker = if index == selected { ">" } else { " " };
            let mut cells = vec![marker.to_string()];
            cells.extend(row.cells.clone());
            cells
        })
        .collect::<Vec<_>>();
    let mut selector_columns = vec![Column {
        header: " ",
        min: 1,
        max: 1,
        align: Align::Left,
    }];
    selector_columns.extend(columns.iter().map(|column| Column {
        header: column.header,
        min: column.min,
        max: column.max,
        align: if column.align_right {
            Align::Right
        } else {
            Align::Left
        },
    }));
    out.push_str(&ui.pair(
        |ui| ui.table("Select", &selector_columns, &row_cells, Some(ui.width)),
        |ui| {
            let details_panel = rows
                .get(selected)
                .map(|current| {
                    ui.details_panel(
                        "Details",
                        &format!(
                            "{}\n\nNext:\nPress Enter to open the highlighted row.\nPress Esc to return without making changes.",
                            current.detail
                        ),
                    )
                })
                .unwrap_or_default();
            ui.flow(&[
                ui.operator_context(
                    if breadcrumb.is_empty() { "-" } else { breadcrumb },
                    scope,
                    "read-only until a workflow is opened",
                    "open the highlighted row",
                ),
                if summary_rows.is_empty() {
                    String::new()
                } else {
                    ui.kv_table("Summary", summary_rows, Some(ui.width))
                },
                details_panel,
            ])
        },
    ));
    out.push_str(&ui.footer(footer));
    out
}

pub(crate) fn render_health_report(report: &HealthReport) -> String {
    let ui = Ui::detect();
    let mut out = ui.banner("EMMC-LAB HEALTH");
    out.push_str(&ui.pair(
        |ui| {
            ui.operator_context(
                "Device Health",
                "selected block device",
                "read-only",
                "not used on this screen",
            )
        },
        |ui| {
            ui.details_panel(
                "Details",
                "Health output combines host context, eMMC telemetry, and capability checks. Missing mmc-utils support stays visible.",
            )
        },
    ));
    let system_rows = system_rows(&report.system);
    let emmc_rows = vec![
        (
            "Device".to_string(),
            report.emmc.device.display().to_string(),
        ),
        (
            "Telemetry".to_string(),
            if report.emmc.available {
                "available".to_string()
            } else {
                "unavailable".to_string()
            },
        ),
        (
            "Source".to_string(),
            opt_text(report.emmc.ext_csd_source.as_deref()),
        ),
        (
            "EXT_CSD_REV".to_string(),
            report
                .emmc
                .ext_csd_rev
                .map(|value| value.to_string())
                .unwrap_or_else(|| "-".to_string()),
        ),
        (
            "DEVICE_LIFE_TIME_EST_TYP_A".to_string(),
            opt_text(report.emmc.device_life_time_est_typ_a.as_deref()),
        ),
        (
            "DEVICE_LIFE_TIME_EST_TYP_B".to_string(),
            opt_text(report.emmc.device_life_time_est_typ_b.as_deref()),
        ),
        (
            "PRE_EOL_INFO".to_string(),
            opt_text(report.emmc.pre_eol_info.as_deref()),
        ),
        (
            "SEC_COUNT".to_string(),
            report
                .emmc
                .sec_count
                .map(|value| value.to_string())
                .unwrap_or_else(|| "-".to_string()),
        ),
        (
            "BKOPS".to_string(),
            report
                .emmc
                .bkops_status
                .map(|value| {
                    format!(
                        "0x{value:02x} ({})",
                        opt_text(report.emmc.bkops_urgency.as_deref())
                    )
                })
                .unwrap_or_else(|| "-".to_string()),
        ),
        (
            "Cache".to_string(),
            match (report.emmc.cache_size_kb, report.emmc.cache_ctrl) {
                (Some(size_kb), Some(ctrl)) => format!("{size_kb} KiB / ctrl {ctrl}"),
                _ => "-".to_string(),
            },
        ),
        (
            "Erase Block".to_string(),
            report
                .emmc
                .erase_group_size_bytes
                .map(fmt_bytes)
                .unwrap_or_else(|| "-".to_string()),
        ),
        (
            "FW".to_string(),
            opt_text(report.emmc.firmware_version.as_deref()),
        ),
    ];
    out.push_str(&ui.pair(
        |ui| ui.kv_table("System", &system_rows, Some(ui.width)),
        |ui| ui.kv_table("eMMC Health", &emmc_rows, Some(ui.width)),
    ));
    let health_meaning_rows = health_meaning_rows(report);
    if !health_meaning_rows.is_empty() {
        out.push_str(&ui.table(
            "Health Meaning",
            &[
                Column {
                    header: "Field",
                    min: 14,
                    max: 28,
                    align: Align::Left,
                },
                Column {
                    header: "Raw",
                    min: 8,
                    max: 16,
                    align: Align::Left,
                },
                Column {
                    header: "Meaning",
                    min: 22,
                    max: 64,
                    align: Align::Left,
                },
            ],
            &health_meaning_rows,
            None,
        ));
    }
    if let Some(decoded) = &report.emmc.ext_csd_decoded {
        out.push_str(&ui.kv_table(
            "MMC Features",
            &ext_csd_feature_rows(decoded),
            Some(ui.width),
        ));
    }
    if let Some(partitions) = &report.emmc.partition_info {
        out.push_str(&ui.kv_table(
            "MMC Partitions",
            &partition_info_rows(partitions),
            Some(ui.width),
        ));
    }
    if let Some(csd) = &report.emmc.csd {
        out.push_str(&ui.kv_table("CSD", &csd_rows(csd), Some(ui.width)));
    }
    if let Some(discard) = &report.discard {
        out.push_str(&ui.kv_table("Discard", &discard_rows(discard), Some(ui.width)));
    }
    out.push_str(&ui.note("Note:", &report.emmc.note));
    if let Some(cid) = &report.emmc.cid {
        out.push_str(&ui.note(
            "CID:",
            &format!(
                "MID=0x{:02x} OID={} PNM={} PRV={} PSN={} MDT={}",
                cid.mid, cid.oid, cid.pnm, cid.prv, cid.psn, cid.mdt
            ),
        ));
    }
    if let Some(vendor) = &report.emmc.vendor_info {
        let detail = match vendor {
            crate::health::VendorSpecificInfo::Samsung { health_data }
            | crate::health::VendorSpecificInfo::SanDisk { health_data }
            | crate::health::VendorSpecificInfo::Micron { health_data }
            | crate::health::VendorSpecificInfo::Hynix { health_data } => health_data
                .iter()
                .map(|(key, value)| format!("{key}={value}"))
                .collect::<Vec<_>>()
                .join(", "),
            crate::health::VendorSpecificInfo::Unknown { raw_bytes } => {
                format!("{} raw vendor bytes", raw_bytes.len())
            }
        };
        out.push_str(&ui.note("Vendor:", &detail));
    }
    if !report.emmc.health_summary.is_empty() {
        out.push_str(&ui.kv_table("Health Summary", &report.emmc.health_summary, None));
    }
    if let Some(raw) = &report.emmc.raw_text {
        out.push_str(&ui.note(
            "Raw EXT_CSD:",
            &truncate_text(raw, ui.width.saturating_sub(20)),
        ));
    }
    out.push_str(&ui.kv_table("Capabilities", &capability_rows(&report.capabilities), None));
    out
}

fn health_meaning_rows(report: &HealthReport) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    if let Some(raw) = report.emmc.device_life_time_est_typ_a.as_deref() {
        rows.push(vec![
            "DEVICE_LIFE_TIME_EST_TYP_A".to_string(),
            raw.to_string(),
            parse_hex_or_decimal_u8(raw)
                .map(decode_life_time_estimate)
                .unwrap_or_else(|| "lifetime bucket unknown".to_string()),
        ]);
    }
    if let Some(raw) = report.emmc.device_life_time_est_typ_b.as_deref() {
        rows.push(vec![
            "DEVICE_LIFE_TIME_EST_TYP_B".to_string(),
            raw.to_string(),
            parse_hex_or_decimal_u8(raw)
                .map(decode_life_time_estimate)
                .unwrap_or_else(|| "lifetime bucket unknown".to_string()),
        ]);
    }
    if let Some(raw) = report.emmc.pre_eol_info.as_deref() {
        rows.push(vec![
            "PRE_EOL_INFO".to_string(),
            raw.to_string(),
            parse_hex_or_decimal_u8(raw)
                .map(|value| decode_pre_eol_info(value).to_string())
                .unwrap_or_else(|| "pre-EOL state unknown".to_string()),
        ]);
    }
    if let Some(status) = report.emmc.bkops_status {
        rows.push(vec![
            "BKOPS_STATUS".to_string(),
            format!("0x{status:02x}"),
            decode_bkops_status(status, report.emmc.bkops_urgency.as_deref()),
        ]);
    }
    if let Some(cid) = &report.emmc.cid {
        rows.push(vec![
            "Manufactured".to_string(),
            cid.mdt.clone(),
            "device manufacturing date from CID".to_string(),
        ]);
        rows.push(vec![
            "Product".to_string(),
            cid.pnm.clone(),
            "device product name from CID".to_string(),
        ]);
    }
    rows
}

fn decode_bkops_status(status: u8, fallback: Option<&str>) -> String {
    match status {
        0x00 => "no background operations needed".to_string(),
        0x01 => "background operations outstanding but low urgency".to_string(),
        0x02 => "background operations may affect performance".to_string(),
        0x03 => "high background-operation urgency".to_string(),
        other => fallback
            .map(str::to_string)
            .unwrap_or_else(|| format!("device-specific status 0x{other:02x}")),
    }
}

fn ext_csd_feature_rows(decoded: &ExtCsdDecoded) -> Vec<(String, String)> {
    vec![
        ("FW".to_string(), decoded.firmware_version.clone()),
        (
            "Boot size".to_string(),
            format!("{} KiB", decoded.boot_size_mult as u64 * 128),
        ),
        (
            "RPMB size".to_string(),
            format!("{} KiB", decoded.rpmb_size_mult as u64 * 128),
        ),
        (
            "Partition config".to_string(),
            format!("0x{:02x}", decoded.partition_config),
        ),
        (
            "Reset function".to_string(),
            format!("0x{:02x}", decoded.rst_n_function),
        ),
        (
            "Bus width".to_string(),
            format!("0x{:02x}", decoded.bus_width),
        ),
        (
            "HS timing".to_string(),
            format!("0x{:02x}", decoded.hs_timing),
        ),
        (
            "CMDQ".to_string(),
            if decoded.cmdq_support != 0 {
                format!("supported, depth {}", decoded.cmdq_depth)
            } else {
                "not supported".to_string()
            },
        ),
        (
            "Reliable write".to_string(),
            format!(
                "rel_wr_sec_c={} wr_rel_set=0x{:02x} wr_rel_param=0x{:02x}",
                decoded.rel_wr_sec_c, decoded.wr_rel_set, decoded.wr_rel_param
            ),
        ),
        (
            "Secure ops".to_string(),
            format!(
                "support=0x{:02x} trim_mult={} erase_mult={}",
                decoded.secure_feature_support, decoded.sec_trim_mult, decoded.sec_erase_mult
            ),
        ),
    ]
}

fn partition_info_rows(info: &crate::health::MmcPartitionInfo) -> Vec<(String, String)> {
    let mut rows = vec![
        (
            "Boot partition 1".to_string(),
            format!("{} KiB", info.boot_partition_1_size_kb),
        ),
        (
            "Boot partition 2".to_string(),
            format!("{} KiB", info.boot_partition_2_size_kb),
        ),
        ("RPMB".to_string(), format!("{} KiB", info.rpmb_size_kb)),
        (
            "Boot enabled".to_string(),
            bool_label(info.boot_partition_enabled),
        ),
        ("Boot ACK".to_string(), bool_label(info.boot_ack_enabled)),
        (
            "Access partition".to_string(),
            info.access_partition.to_string(),
        ),
    ];
    if !info.gp_partitions.is_empty() {
        rows.push((
            "General purpose".to_string(),
            info.gp_partitions
                .iter()
                .map(|part| format!("GP{} mult={:?}", part.index, part.size_mult))
                .collect::<Vec<_>>()
                .join(", "),
        ));
    }
    if let Some(bytes) = info.enhanced_area_size_bytes {
        rows.push(("Enhanced area".to_string(), fmt_bytes(bytes)));
    }
    rows
}

fn csd_rows(csd: &crate::health::CsdInfo) -> Vec<(String, String)> {
    vec![
        ("CSD structure".to_string(), csd.csd_structure.to_string()),
        ("Spec vers".to_string(), csd.spec_vers.to_string()),
        (
            "Tran speed".to_string(),
            format!("0x{:02x}", csd.tran_speed),
        ),
        ("CCC".to_string(), format!("0x{:03x}", csd.ccc)),
        ("Read block len".to_string(), csd.read_bl_len.to_string()),
        ("Write block len".to_string(), csd.write_bl_len.to_string()),
        ("Erase group".to_string(), csd.erase_grp_size.to_string()),
        ("WP group".to_string(), csd.wp_grp_size.to_string()),
        (
            "Write protect".to_string(),
            format!(
                "perm={} temp={} enabled={}",
                bool_label(csd.perm_write_protect),
                bool_label(csd.tmp_write_protect),
                bool_label(csd.wp_grp_enable)
            ),
        ),
    ]
}

fn discard_rows(discard: &crate::discard::DiscardCapability) -> Vec<(String, String)> {
    vec![
        (
            "Supported".to_string(),
            bool_label(discard.discard_supported),
        ),
        (
            "Granularity".to_string(),
            fmt_bytes(discard.discard_granularity),
        ),
        (
            "Max bytes".to_string(),
            fmt_bytes(discard.discard_max_bytes),
        ),
        (
            "blkdiscard".to_string(),
            bool_label(discard.blkdiscard_available),
        ),
        ("fstrim".to_string(), bool_label(discard.fstrim_available)),
        (
            "BLKDISCARD ioctl".to_string(),
            bool_label(discard.blkdiscard_ioctl_viable),
        ),
    ]
}

fn parse_hex_or_decimal_u8(raw: &str) -> Option<u8> {
    let trimmed = raw.trim();
    if let Some(hex) = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
    {
        u8::from_str_radix(hex, 16).ok()
    } else {
        trimmed.parse::<u8>().ok()
    }
}

pub(crate) fn render_live_monitor(
    report: &crate::diagnostics::DiagnosticReport,
    interval_ms: u64,
) -> String {
    let ui = Ui::detect();
    let mut out = ui.banner("EMMC-LAB LIVE MONITOR");
    // Switch to reduced layouts earlier so the header and top process table stay visible
    // on shorter SSH terminal windows instead of scrolling off the screen.
    let compact = ui.height <= 44;
    let medium = ui.height <= 60;
    let overview_rows = vec![
        ("Updated".to_string(), report.ended_at.to_rfc3339()),
        (
            "Duration".to_string(),
            format!("{} s", report.duration_seconds),
        ),
        ("Refresh".to_string(), format!("{} ms", interval_ms.max(1))),
        (
            "Observed".to_string(),
            format!(
                "{} proc, {} files, {} dirs, {} dev",
                report.top_processes.len(),
                report.top_files.len(),
                report.top_directories.len(),
                report.device_totals.len()
            ),
        ),
    ];
    let note_rows = vec![
        (
            "Bytes".to_string(),
            "logical and storage bytes shown separately".to_string(),
        ),
        (
            "Paths".to_string(),
            format!("unresolved {}", report.unresolved_path_count),
        ),
        (
            "Dropped".to_string(),
            report.dropped_event_count.to_string(),
        ),
        (
            "Restricted".to_string(),
            report.restricted_process_count.to_string(),
        ),
        (
            "Kernel".to_string(),
            report.kernel_process_count.to_string(),
        ),
        (
            "Unattrib R".to_string(),
            fmt_bytes(report.unattributed_storage_read_bytes),
        ),
        (
            "Unattrib W".to_string(),
            fmt_bytes(report.unattributed_storage_write_bytes),
        ),
        (
            "Proc>Dev R".to_string(),
            fmt_bytes(report.process_excess_storage_read_bytes),
        ),
        (
            "Proc>Dev W".to_string(),
            fmt_bytes(report.process_excess_storage_write_bytes),
        ),
    ];
    let block_rate_rows = report
        .block_stats_timeline
        .iter()
        .take(6)
        .map(|delta| {
            vec![
                format!("/dev/{}", delta.device),
                format!("{:.1}/{:.1}", delta.read_iops, delta.write_iops),
                format!(
                    "{}/{}",
                    fmt_bytes_f64(delta.read_throughput_bytes_per_sec),
                    fmt_bytes_f64(delta.write_throughput_bytes_per_sec)
                ),
                format!("{:.1}%", delta.utilization_percent),
                format!("{:.2}", delta.avg_queue_depth),
            ]
        })
        .collect::<Vec<_>>();
    out.push_str(&ui.pair(
        |ui| {
            ui.operator_context(
                "Diagnostics / Live Monitor",
                "live procfs monitor",
                "read-only best-effort monitor",
                "not used during refresh; q quits",
            )
        },
        |ui| ui.kv_table("Monitor", &overview_rows, Some(ui.width)),
    ));
    out.push_str(&ui.kv_table("Notes", &note_rows, None));

    let process_limit = live_process_limit(ui.width, ui.height);
    let process_rows = report
        .top_processes
        .iter()
        .take(process_limit)
        .map(|process| {
            vec![
                process.pid.to_string(),
                process.command.clone(),
                fmt_rate(process.logical_read_bytes_per_sec),
                fmt_rate(process.logical_write_bytes_per_sec),
                fmt_rate(process.storage_read_bytes_per_sec),
                fmt_rate(process.storage_write_bytes_per_sec),
                fmt_bytes(process.storage_read_bytes),
                fmt_bytes(process.storage_write_bytes),
            ]
        })
        .collect::<Vec<_>>();
    let process_panel = if process_rows.is_empty() {
        String::new()
    } else {
        ui.table(
            "Top Processes",
            &[
                Column {
                    header: "PID",
                    min: 5,
                    max: 7,
                    align: Align::Right,
                },
                Column {
                    header: "Command",
                    min: 10,
                    max: 20,
                    align: Align::Left,
                },
                Column {
                    header: "Log R/s",
                    min: 10,
                    max: 12,
                    align: Align::Right,
                },
                Column {
                    header: "Log W/s",
                    min: 10,
                    max: 12,
                    align: Align::Right,
                },
                Column {
                    header: "Store R/s",
                    min: 10,
                    max: 12,
                    align: Align::Right,
                },
                Column {
                    header: "Store W/s",
                    min: 10,
                    max: 12,
                    align: Align::Right,
                },
                Column {
                    header: "Store R",
                    min: 10,
                    max: 11,
                    align: Align::Right,
                },
                Column {
                    header: "Store W",
                    min: 10,
                    max: 11,
                    align: Align::Right,
                },
            ],
            &process_rows,
            None,
        )
    };

    if compact {
        out.push_str(&process_panel);
        if !block_rate_rows.is_empty() {
            out.push_str(&ui.table("Block Rates", &block_rate_columns(), &block_rate_rows, None));
        }
        if report.restricted_process_count > 0 || report.kernel_process_count > 0 {
            out.push_str(&ui.note(
                "Access:",
                &format!(
                    "{} restricted, {} kernel-like; run with sudo for fuller attribution",
                    report.restricted_process_count, report.kernel_process_count
                ),
            ));
        }
        out.push_str(&ui.note(
            "Compact:",
            "lower sections hidden on short screens; use a taller terminal or timed capture for full detail",
        ));
        out.push_str(&ui.note(
            "Attribution:",
            "best-effort procfs monitor; logical and storage bytes remain separate",
        ));
        out.push_str(&ui.footer("q quit the monitor"));
        return out;
    }

    let device_rows = report
        .device_totals
        .iter()
        .take(live_device_limit(ui.width, ui.height))
        .map(|device| {
            vec![
                device.device.clone(),
                device.reads_completed.to_string(),
                device.writes_completed.to_string(),
                device.current_ios_in_progress.to_string(),
            ]
        })
        .collect::<Vec<_>>();
    let device_panel = if device_rows.is_empty() {
        String::new()
    } else {
        ui.table(
            "Devices",
            &[
                Column {
                    header: "Device",
                    min: 10,
                    max: 14,
                    align: Align::Left,
                },
                Column {
                    header: "Reads",
                    min: 7,
                    max: 9,
                    align: Align::Right,
                },
                Column {
                    header: "Writes",
                    min: 7,
                    max: 9,
                    align: Align::Right,
                },
                Column {
                    header: "InFlight",
                    min: 8,
                    max: 9,
                    align: Align::Right,
                },
            ],
            &device_rows,
            None,
        )
    };
    let restricted_limit = if medium { 4 } else { 8 };
    let restricted_rows = report
        .restricted_processes
        .iter()
        .take(restricted_limit)
        .map(|process| {
            vec![
                process.pid.to_string(),
                process.user.clone(),
                process.command.clone(),
                process.reason.replace('_', " "),
            ]
        })
        .collect::<Vec<_>>();
    let restricted_panel = if restricted_rows.is_empty() {
        String::new()
    } else {
        ui.table(
            "Restricted / Kernel Activity",
            &[
                Column {
                    header: "PID",
                    min: 5,
                    max: 7,
                    align: Align::Right,
                },
                Column {
                    header: "User",
                    min: 6,
                    max: 10,
                    align: Align::Left,
                },
                Column {
                    header: "Command",
                    min: 12,
                    max: 24,
                    align: Align::Left,
                },
                Column {
                    header: "Reason",
                    min: 10,
                    max: 22,
                    align: Align::Left,
                },
            ],
            &restricted_rows,
            None,
        )
    };
    let kernel_rows = report
        .kernel_activity
        .iter()
        .take(if medium { 3 } else { 6 })
        .map(|bucket| {
            vec![
                bucket.category.clone(),
                bucket.process_count.to_string(),
                bucket.sample_commands.join(", "),
            ]
        })
        .collect::<Vec<_>>();
    let kernel_panel = if kernel_rows.is_empty() {
        String::new()
    } else {
        ui.table(
            "Kernel / Writeback",
            &[
                Column {
                    header: "Class",
                    min: 8,
                    max: 10,
                    align: Align::Left,
                },
                Column {
                    header: "Count",
                    min: 5,
                    max: 7,
                    align: Align::Right,
                },
                Column {
                    header: "Sample Commands",
                    min: 18,
                    max: 36,
                    align: Align::Left,
                },
            ],
            &kernel_rows,
            None,
        )
    };
    if !process_panel.is_empty() || !device_panel.is_empty() {
        out.push_str(&ui.pair(
            |ui| {
                if process_rows.is_empty() {
                    String::new()
                } else {
                    ui.table(
                        "Top Processes",
                        &[
                            Column {
                                header: "PID",
                                min: 5,
                                max: 7,
                                align: Align::Right,
                            },
                            Column {
                                header: "Command",
                                min: 10,
                                max: 20,
                                align: Align::Left,
                            },
                            Column {
                                header: "Log R/s",
                                min: 10,
                                max: 12,
                                align: Align::Right,
                            },
                            Column {
                                header: "Log W/s",
                                min: 10,
                                max: 12,
                                align: Align::Right,
                            },
                            Column {
                                header: "Store R/s",
                                min: 10,
                                max: 12,
                                align: Align::Right,
                            },
                            Column {
                                header: "Store W/s",
                                min: 10,
                                max: 12,
                                align: Align::Right,
                            },
                            Column {
                                header: "Store R",
                                min: 10,
                                max: 11,
                                align: Align::Right,
                            },
                            Column {
                                header: "Store W",
                                min: 10,
                                max: 11,
                                align: Align::Right,
                            },
                        ],
                        &process_rows,
                        Some(ui.width),
                    )
                }
            },
            |ui| {
                if device_rows.is_empty() {
                    String::new()
                } else {
                    ui.table(
                        "Devices",
                        &[
                            Column {
                                header: "Device",
                                min: 10,
                                max: 14,
                                align: Align::Left,
                            },
                            Column {
                                header: "Reads",
                                min: 7,
                                max: 9,
                                align: Align::Right,
                            },
                            Column {
                                header: "Writes",
                                min: 7,
                                max: 9,
                                align: Align::Right,
                            },
                            Column {
                                header: "InFlight",
                                min: 8,
                                max: 9,
                                align: Align::Right,
                            },
                        ],
                        &device_rows,
                        Some(ui.width),
                    )
                }
            },
        ));
    }
    if !block_rate_rows.is_empty() {
        out.push_str(&ui.table("Block Rates", &block_rate_columns(), &block_rate_rows, None));
    }
    if !restricted_panel.is_empty() {
        out.push_str(&restricted_panel);
    }
    if !kernel_panel.is_empty() {
        out.push_str(&kernel_panel);
    }

    if !medium {
        let path_rows = report
            .top_processes
            .iter()
            .take(live_process_limit(ui.width, ui.height).min(5))
            .map(|process| {
                vec![
                    process.pid.to_string(),
                    process.exe_path.clone(),
                    process.cwd.clone(),
                    process.hottest_file.clone(),
                ]
            })
            .collect::<Vec<_>>();
        if !path_rows.is_empty() {
            out.push_str(&ui.table(
                "Process Paths",
                &[
                    Column {
                        header: "PID",
                        min: 5,
                        max: 7,
                        align: Align::Right,
                    },
                    Column {
                        header: "Executable",
                        min: 14,
                        max: 28,
                        align: Align::Left,
                    },
                    Column {
                        header: "CWD",
                        min: 14,
                        max: 24,
                        align: Align::Left,
                    },
                    Column {
                        header: "Hottest File",
                        min: 14,
                        max: 28,
                        align: Align::Left,
                    },
                ],
                &path_rows,
                None,
            ));
        }
    }

    if ui.height > 48 {
        let file_rows = report
            .top_files
            .iter()
            .take(live_file_limit(ui.width, ui.height))
            .map(|file| {
                vec![
                    file.file_path.clone(),
                    file.last_pid_touching_file.to_string(),
                    fmt_bytes(file.read_bytes),
                    fmt_bytes(file.write_bytes),
                    file.last_seen_offset_bytes
                        .map(fmt_bytes)
                        .unwrap_or_else(|| "-".to_string()),
                    file.last_seen_open_flags.clone(),
                ]
            })
            .collect::<Vec<_>>();
        if !file_rows.is_empty() {
            out.push_str(&ui.table(
                "Top Files",
                &[
                    Column {
                        header: "File",
                        min: 16,
                        max: 36,
                        align: Align::Left,
                    },
                    Column {
                        header: "PID",
                        min: 5,
                        max: 7,
                        align: Align::Right,
                    },
                    Column {
                        header: "Read",
                        min: 10,
                        max: 11,
                        align: Align::Right,
                    },
                    Column {
                        header: "Write",
                        min: 10,
                        max: 11,
                        align: Align::Right,
                    },
                    Column {
                        header: "FD Pos",
                        min: 8,
                        max: 11,
                        align: Align::Right,
                    },
                    Column {
                        header: "Flags",
                        min: 8,
                        max: 14,
                        align: Align::Left,
                    },
                ],
                &file_rows,
                None,
            ));
        }
    }

    out.push_str(&ui.note(
        "Attribution:",
        "best-effort procfs monitor; logical and storage bytes remain separate; run with sudo for fuller process attribution",
    ));
    out.push_str(&ui.footer("q quit the monitor"));
    out
}

pub(crate) fn render_doctor_report(report: &DoctorReport) -> String {
    let ui = Ui::detect();
    let mut out = ui.banner("EMMC-LAB DOCTOR");
    out.push_str(&ui.pair(
        |ui| {
            ui.operator_context(
                "Settings / Doctor",
                "environment validation",
                "read-only",
                "not used on this screen",
            )
        },
        |ui| {
            ui.details_panel(
                "Details",
                "Doctor keeps capability checks and visible devices together so operators can validate the environment before running tests.",
            )
        },
    ));
    let generated = vec![("Generated".to_string(), report.generated_at.to_rfc3339())];
    let check_rows = report
        .checks
        .iter()
        .map(|check| {
            vec![
                if check.ok {
                    "OK".to_string()
                } else {
                    "WARN".to_string()
                },
                check.name.clone(),
                check.detail.clone(),
            ]
        })
        .collect::<Vec<_>>();
    out.push_str(&ui.pair(
        |ui| ui.kv_table("Run", &generated, Some(ui.width)),
        |ui| {
            ui.kv_table(
                "Capabilities",
                &capability_rows(&report.capabilities),
                Some(ui.width),
            )
        },
    ));
    out.push_str(&ui.table(
        "Checks",
        &[
            Column {
                header: "Status",
                min: 6,
                max: 6,
                align: Align::Left,
            },
            Column {
                header: "Check",
                min: 16,
                max: 24,
                align: Align::Left,
            },
            Column {
                header: "Detail",
                min: 24,
                max: 58,
                align: Align::Left,
            },
        ],
        &check_rows,
        None,
    ));

    let visible_devices = primary_devices(&report.devices);
    let hidden_count = report.devices.len().saturating_sub(visible_devices.len());
    let device_rows = visible_devices
        .iter()
        .take(row_limit(ui.height) + 2)
        .map(device_row)
        .collect::<Vec<_>>();
    if !device_rows.is_empty() {
        out.push_str(&ui.table("Primary Devices", &device_columns(), &device_rows, None));
    }
    let geometry_rows = visible_devices
        .iter()
        .filter_map(device_geometry_row)
        .collect::<Vec<_>>();
    if !geometry_rows.is_empty() {
        out.push_str(&ui.table(
            "Geometry Highlights",
            &geometry_columns(),
            &geometry_rows,
            None,
        ));
    }
    if hidden_count > 0 {
        out.push_str(&ui.note(
            "Devices:",
            &format!(
                "{} auxiliary devices hidden here; use `emmc-lab list-devices` for the full list.",
                hidden_count
            ),
        ));
    }
    out
}

pub(crate) fn render_settings(
    paths: &AppPaths,
    system: &SystemSnapshot,
    caps: &CapabilityReport,
) -> String {
    let ui = Ui::detect();
    let mut out = ui.banner("EMMC-LAB SETTINGS");
    out.push_str(&ui.pair(
        |ui| {
            ui.operator_context(
                "Settings",
                "local environment facts",
                "read-only",
                "not used on this screen",
            )
        },
        |ui| {
            ui.details_panel(
                "Details",
                "Settings shows stable paths, current host facts, and capability status without changing device state.",
            )
        },
    ));
    let path_rows = vec![
        (
            "Profiles".to_string(),
            paths.profiles_dir.display().to_string(),
        ),
        (
            "Sessions".to_string(),
            paths.sessions_dir.display().to_string(),
        ),
        (
            "Exports".to_string(),
            paths.exports_dir.display().to_string(),
        ),
        (
            "Samples".to_string(),
            paths.samples_dir.display().to_string(),
        ),
    ];
    out.push_str(&ui.pair(
        |ui| ui.kv_table("Paths", &path_rows, Some(ui.width)),
        |ui| ui.kv_table("System", &system_rows(system), Some(ui.width)),
    ));
    out.push_str(&ui.kv_table("Capabilities", &capability_rows(caps), None));
    out
}

pub(crate) fn render_device_list(devices: &[DeviceInfo]) -> String {
    let ui = Ui::detect();
    let mut out = ui.banner("EMMC-LAB DEVICES");
    out.push_str(&ui.pair(
        |ui| {
            ui.operator_context(
                "Settings / Devices",
                "device inventory",
                "read-only",
                "not used on this screen",
            )
        },
        |ui| {
            ui.details_panel(
                "Details",
                "Device list is ordered for operator review. Use the health workflow or run wizard to act on one of these targets.",
            )
        },
    ));
    let rows = devices.iter().map(device_row).collect::<Vec<_>>();
    out.push_str(&ui.table("Visible Devices", &device_columns(), &rows, None));
    let geometry_rows = devices
        .iter()
        .filter_map(device_geometry_row)
        .collect::<Vec<_>>();
    if !geometry_rows.is_empty() {
        out.push_str(&ui.table(
            "Geometry Highlights",
            &geometry_columns(),
            &geometry_rows,
            None,
        ));
    }
    out
}

pub(crate) fn render_device_geometry(geometry: &BlockGeometry) -> String {
    let ui = Ui::detect();
    let mut out = ui.banner("EMMC-LAB GEOMETRY");
    out.push_str(&ui.pair(
        |ui| {
            ui.operator_context(
                "Geometry",
                "selected block device",
                "read-only",
                "not used on this screen",
            )
        },
        |ui| {
            ui.details_panel(
                "Details",
                "Geometry shows queue sizing, discard support, alignment, and scheduler details for the selected block device.",
            )
        },
    ));
    let rows = vec![
        ("Device".to_string(), geometry.device.display().to_string()),
        (
            "Logical block size".to_string(),
            format_bytes_exact(geometry.logical_block_size),
        ),
        (
            "Physical block size".to_string(),
            format_bytes_exact(geometry.physical_block_size),
        ),
        (
            "Minimum I/O size".to_string(),
            format_bytes_exact(geometry.minimum_io_size),
        ),
        (
            "Optimal I/O size".to_string(),
            format_bytes_exact(geometry.optimal_io_size),
        ),
        (
            "Discard granularity".to_string(),
            format_bytes_exact(geometry.discard_granularity),
        ),
        (
            "Discard max".to_string(),
            format_bytes_exact(geometry.discard_max_bytes),
        ),
        (
            "Discard zeroes data".to_string(),
            geometry
                .discard_zeroes_data
                .map(bool_label)
                .unwrap_or_else(|| "-".to_string()),
        ),
        (
            "Partition start LBA".to_string(),
            geometry
                .partition_start_lba
                .map(|value| value.to_string())
                .unwrap_or_else(|| "-".to_string()),
        ),
        (
            "Alignment offset".to_string(),
            format_bytes_exact(geometry.alignment_offset),
        ),
        ("Read-only".to_string(), bool_label(geometry.read_only)),
        ("Removable".to_string(), bool_label(geometry.removable)),
        (
            "Write cache".to_string(),
            opt_text(geometry.write_cache.as_deref()),
        ),
        (
            "Scheduler".to_string(),
            opt_text(geometry.scheduler.as_deref()),
        ),
        (
            "Size sectors".to_string(),
            format_grouped_u64(geometry.size_sectors),
        ),
        ("Size bytes".to_string(), fmt_bytes(geometry.size_bytes)),
    ];
    out.push_str(&ui.kv_table("Block Geometry", &rows, None));
    out
}

fn system_rows(system: &SystemSnapshot) -> Vec<(String, String)> {
    vec![
        (
            "Kernel".to_string(),
            opt_text(system.kernel_version.as_deref()),
        ),
        ("Hostname".to_string(), opt_text(system.hostname.as_deref())),
        (
            "CPU Temp".to_string(),
            system
                .cpu_temperature_c
                .map(|value| format!("{value:.1} C"))
                .unwrap_or_else(|| "-".to_string()),
        ),
        (
            "CPU Freq".to_string(),
            system
                .cpu_frequency_mhz
                .map(|value| format!("{value} MHz"))
                .unwrap_or_else(|| "-".to_string()),
        ),
        (
            "Memory Total".to_string(),
            system
                .mem_total_kb
                .map(|value| fmt_bytes(value.saturating_mul(1024)))
                .unwrap_or_else(|| "-".to_string()),
        ),
        (
            "Memory Available".to_string(),
            system
                .mem_available_kb
                .map(|value| fmt_bytes(value.saturating_mul(1024)))
                .unwrap_or_else(|| "-".to_string()),
        ),
        (
            "Load Avg".to_string(),
            opt_text(system.load_average.as_deref()),
        ),
    ]
}

fn capability_rows(caps: &CapabilityReport) -> Vec<(String, String)> {
    vec![
        ("io_uring".to_string(), bool_label(caps.io_uring_available)),
        (
            "Direct I/O".to_string(),
            bool_label(caps.direct_io_available),
        ),
        (
            "Deep Trace".to_string(),
            bool_label(caps.deep_trace_available),
        ),
        (
            "MMC Health".to_string(),
            bool_label(caps.mmc_health_available),
        ),
        ("procfs".to_string(), bool_label(caps.procfs_access)),
        ("Permission".to_string(), caps.permission_level.clone()),
    ]
}

fn device_columns<'a>() -> [Column<'a>; 7] {
    [
        Column {
            header: "Device",
            min: 10,
            max: 14,
            align: Align::Left,
        },
        Column {
            header: "Type",
            min: 8,
            max: 10,
            align: Align::Left,
        },
        Column {
            header: "Model",
            min: 8,
            max: 18,
            align: Align::Left,
        },
        Column {
            header: "Size",
            min: 8,
            max: 10,
            align: Align::Right,
        },
        Column {
            header: "Mounted",
            min: 7,
            max: 7,
            align: Align::Left,
        },
        Column {
            header: "Root",
            min: 4,
            max: 4,
            align: Align::Left,
        },
        Column {
            header: "Mounts",
            min: 12,
            max: 34,
            align: Align::Left,
        },
    ]
}

fn geometry_columns<'a>() -> [Column<'a>; 6] {
    [
        Column {
            header: "Device",
            min: 10,
            max: 14,
            align: Align::Left,
        },
        Column {
            header: "LBS",
            min: 7,
            max: 11,
            align: Align::Right,
        },
        Column {
            header: "PBS",
            min: 7,
            max: 11,
            align: Align::Right,
        },
        Column {
            header: "Min/Opt I/O",
            min: 14,
            max: 18,
            align: Align::Left,
        },
        Column {
            header: "Discard",
            min: 10,
            max: 18,
            align: Align::Left,
        },
        Column {
            header: "Cache/Sched",
            min: 14,
            max: 24,
            align: Align::Left,
        },
    ]
}

fn block_rate_columns<'a>() -> [Column<'a>; 5] {
    [
        Column {
            header: "Device",
            min: 10,
            max: 14,
            align: Align::Left,
        },
        Column {
            header: "R/W IOPS",
            min: 10,
            max: 14,
            align: Align::Right,
        },
        Column {
            header: "R/W BW",
            min: 16,
            max: 22,
            align: Align::Right,
        },
        Column {
            header: "Util",
            min: 6,
            max: 7,
            align: Align::Right,
        },
        Column {
            header: "QD",
            min: 4,
            max: 5,
            align: Align::Right,
        },
    ]
}

fn device_row(device: &DeviceInfo) -> Vec<String> {
    vec![
        device.path.display().to_string(),
        device.media_type.clone().unwrap_or_else(|| "-".to_string()),
        opt_text(device.model.as_deref()),
        device
            .size_bytes
            .map(fmt_bytes)
            .unwrap_or_else(|| "-".to_string()),
        bool_label(device.mounted),
        bool_label(device.is_root_device),
        mount_summary(device),
    ]
}

fn device_geometry_row(device: &DeviceInfo) -> Option<Vec<String>> {
    let geometry = device.geometry.as_ref()?;
    Some(vec![
        device.path.display().to_string(),
        format_bytes_exact(geometry.logical_block_size),
        format_bytes_exact(geometry.physical_block_size),
        format!(
            "{}/{}",
            format_bytes_exact(geometry.minimum_io_size),
            format_bytes_exact(geometry.optimal_io_size)
        ),
        format!(
            "{}/{}",
            format_bytes_exact(geometry.discard_granularity),
            format_bytes_exact(geometry.discard_max_bytes)
        ),
        format!(
            "{}/{}",
            geometry.write_cache.as_deref().unwrap_or("-"),
            geometry.scheduler.as_deref().unwrap_or("-")
        ),
    ])
}

fn mount_summary(device: &DeviceInfo) -> String {
    if device.mountpoints.is_empty() {
        return "-".to_string();
    }
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
}

fn format_bytes_exact(value: u64) -> String {
    if value == 0 {
        return "0 B".to_string();
    }
    fmt_bytes(value)
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

fn primary_devices(devices: &[DeviceInfo]) -> Vec<DeviceInfo> {
    devices
        .iter()
        .filter(|device| !device.name.starts_with("loop") && !device.name.starts_with("ram"))
        .cloned()
        .collect()
}

fn diagnostic_device_rank(
    device: &str,
    summary: &crate::diagnostics::DeviceSummary,
) -> (u8, Reverse<u64>, String) {
    let family = if device.contains("mmcblk") {
        0
    } else if device.contains("nvme") || device.contains("/sd") {
        1
    } else if device.contains("loop") {
        3
    } else if device.contains("ram") {
        4
    } else {
        2
    };
    let activity = summary
        .reads_completed
        .saturating_add(summary.writes_completed)
        .saturating_add(summary.sectors_read)
        .saturating_add(summary.sectors_written)
        .saturating_add(summary.current_ios_in_progress);
    (family, Reverse(activity), device.to_string())
}

fn row_limit(height: usize) -> usize {
    height.saturating_sub(12).max(3)
}

fn live_process_limit(_width: usize, height: usize) -> usize {
    let height_limit = row_limit(height);
    if height <= 44 {
        height_limit.min(4)
    } else if height <= 60 {
        height_limit.min(5)
    } else {
        height_limit
    }
}

fn live_device_limit(_width: usize, height: usize) -> usize {
    let height_limit = row_limit(height);
    if height <= 44 {
        height_limit.min(3)
    } else if height <= 60 {
        height_limit.min(4)
    } else {
        height_limit
    }
}

fn live_file_limit(_width: usize, height: usize) -> usize {
    let height_limit = row_limit(height);
    if height <= 56 {
        height_limit.min(4)
    } else {
        height_limit
    }
}

fn stdout_width() -> Option<usize> {
    if unsafe { libc::isatty(libc::STDOUT_FILENO) } != 1 {
        return None;
    }
    let mut size = libc::winsize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let result = unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut size) };
    if result == 0 && size.ws_col > 0 {
        Some(size.ws_col as usize)
    } else {
        None
    }
}

fn stdout_height() -> Option<usize> {
    if unsafe { libc::isatty(libc::STDOUT_FILENO) } != 1 {
        return None;
    }
    let mut size = libc::winsize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let result = unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut size) };
    if result == 0 && size.ws_row > 0 {
        Some(size.ws_row as usize)
    } else {
        None
    }
}

pub(crate) fn set_color_policy(policy: ColorPolicy) {
    COLOR_POLICY.store(policy as u8, Ordering::Relaxed);
}

fn current_color_policy() -> ColorPolicy {
    match COLOR_POLICY.load(Ordering::Relaxed) {
        value if value == ColorPolicy::Always as u8 => ColorPolicy::Always,
        value if value == ColorPolicy::Never as u8 => ColorPolicy::Never,
        _ => ColorPolicy::Auto,
    }
}

fn resolve_color_enabled(
    policy: ColorPolicy,
    no_color: bool,
    term: &str,
    stdout_tty: bool,
) -> bool {
    match policy {
        ColorPolicy::Always => true,
        ColorPolicy::Never => false,
        ColorPolicy::Auto => !no_color && !term.is_empty() && term != "dumb" && stdout_tty,
    }
}

fn truncate_text(value: &str, width: usize) -> String {
    let len = value.chars().count();
    if len <= width {
        return value.to_string();
    }
    if width == 0 {
        return String::new();
    }
    if width == 1 {
        return ELLIPSIS.to_string();
    }
    if should_preserve_tail(value) {
        return abbreviate_middle(value, width);
    }
    let mut out = value.chars().take(width - 1).collect::<String>();
    out.push(ELLIPSIS);
    out
}

fn prepare_inline_display(value: &str) -> String {
    let sanitized = sanitize_inline(value);
    abbreviate_for_display(&sanitized, ABBREVIATED_INLINE_LIMIT)
}

fn abbreviate_for_display(value: &str, max_width: usize) -> String {
    let len = value.chars().count();
    if len <= max_width || max_width == 0 {
        return value.to_string();
    }
    if is_path_like(value) {
        return abbreviate_path_like(value, max_width);
    }
    if should_preserve_tail(value) {
        return abbreviate_middle(value, max_width);
    }
    value.to_string()
}

fn abbreviate_path_like(value: &str, max_width: usize) -> String {
    if max_width <= 1 {
        return ELLIPSIS.to_string();
    }

    let separator = if value.contains('/') { '/' } else { '\\' };
    let Some((prefix, suffix)) = value.rsplit_once(separator) else {
        return abbreviate_middle(value, max_width);
    };
    let suffix_len = suffix.chars().count();
    if suffix_len + 2 >= max_width {
        return abbreviate_middle(suffix, max_width);
    }

    let prefix_budget = max_width.saturating_sub(suffix_len + 2);
    let prefix_head = prefix.chars().take(prefix_budget).collect::<String>();
    format!("{prefix_head}{ELLIPSIS}{separator}{suffix}")
}

fn abbreviate_middle(value: &str, width: usize) -> String {
    let len = value.chars().count();
    if len <= width {
        return value.to_string();
    }
    if width == 0 {
        return String::new();
    }
    if width == 1 {
        return ELLIPSIS.to_string();
    }
    let left = (width - 1) / 2;
    let right = width - 1 - left;
    let head = value.chars().take(left).collect::<String>();
    let tail = value
        .chars()
        .skip(len.saturating_sub(right))
        .collect::<String>();
    format!("{head}{ELLIPSIS}{tail}")
}

fn should_preserve_tail(value: &str) -> bool {
    is_path_like(value) || is_filename_like(value)
}

fn is_path_like(value: &str) -> bool {
    value.contains('/') || value.contains('\\')
}

fn is_filename_like(value: &str) -> bool {
    !value.chars().any(char::is_whitespace) && value.contains('.')
}

fn shrink_widths_to_budget(
    widths: &mut [usize],
    desired: &[usize],
    minimum: &[usize],
    budget: usize,
) {
    if widths.is_empty() {
        return;
    }

    let total = desired.iter().sum::<usize>();
    if total == 0 {
        for width in widths.iter_mut() {
            *width = 1;
        }
        return;
    }
    if total <= budget {
        widths.copy_from_slice(desired);
        return;
    }

    for ((width, desired_width), min_width) in
        widths.iter_mut().zip(desired.iter()).zip(minimum.iter())
    {
        let scaled = desired_width.saturating_mul(budget) / total;
        *width = scaled.max(*min_width);
    }

    let mut total_width = widths.iter().sum::<usize>();
    while total_width > budget {
        let Some((index, _)) = widths
            .iter()
            .enumerate()
            .filter(|(index, width)| **width > minimum[*index])
            .max_by_key(|(index, width)| (**width, desired[*index]))
        else {
            break;
        };
        widths[index] -= 1;
        total_width -= 1;
    }

    if total_width > budget && minimum.iter().any(|min_width| *min_width > 1) {
        let relaxed_minimum = vec![1; widths.len()];
        shrink_widths_to_budget(widths, desired, &relaxed_minimum, budget);
        return;
    }
}

fn sanitize_inline(value: &str) -> String {
    value
        .replace(['\r', '\n', '\t'], " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn visible_width(value: &str) -> usize {
    let mut width = 0;
    let mut chars = value.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' && chars.peek() == Some(&'[') {
            let _ = chars.next();
            while let Some(next) = chars.next() {
                if ('@'..='~').contains(&next) {
                    break;
                }
            }
            continue;
        }
        width += 1;
    }
    width
}

fn opt_text(value: Option<&str>) -> String {
    value
        .filter(|value| !value.is_empty())
        .unwrap_or("-")
        .to_string()
}

fn bool_label(value: bool) -> String {
    if value {
        "yes".to_string()
    } else {
        "no".to_string()
    }
}

fn fmt_bytes(value: u64) -> String {
    let units = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut size = value as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < units.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{value} {}", units[unit])
    } else {
        format!("{size:.1} {}", units[unit])
    }
}

fn fmt_rate(value: f64) -> String {
    format!("{}/s", fmt_bytes_f64(value))
}

fn fmt_bytes_f64(value: f64) -> String {
    let units = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut size = value.max(0.0);
    let mut unit = 0;
    while size >= 1024.0 && unit < units.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{size:.0} {}", units[unit])
    } else {
        format!("{size:.1} {}", units[unit])
    }
}

fn fmt_percent(value: f64) -> String {
    format!("{value:.1}%")
}

fn fmt_us(value: u64) -> String {
    format!("{value} us")
}

fn build_diag_rows(diag: &DiagnosticReport) -> Vec<(String, String)> {
    let mut rows = vec![
        (
            "Mode".to_string(),
            format!("{:?}", diag.mode).to_lowercase(),
        ),
        ("Started".to_string(), diag.started_at.to_rfc3339()),
        ("Ended".to_string(), diag.ended_at.to_rfc3339()),
        (
            "Duration".to_string(),
            format!("{} s", diag.duration_seconds),
        ),
        (
            "Observed".to_string(),
            format!(
                "{} processes, {} files, {} directories, {} devices",
                diag.top_processes.len(),
                diag.top_files.len(),
                diag.top_directories.len(),
                diag.device_totals.len()
            ),
        ),
        (
            "Unresolved Paths".to_string(),
            diag.unresolved_path_count.to_string(),
        ),
        (
            "Dropped Events".to_string(),
            diag.dropped_event_count.to_string(),
        ),
        (
            "Restricted".to_string(),
            diag.restricted_process_count.to_string(),
        ),
        (
            "Kernel-like".to_string(),
            diag.kernel_process_count.to_string(),
        ),
        (
            "Unattrib Read".to_string(),
            fmt_bytes(diag.unattributed_storage_read_bytes),
        ),
        (
            "Unattrib Write".to_string(),
            fmt_bytes(diag.unattributed_storage_write_bytes),
        ),
        (
            "Proc>Dev Read".to_string(),
            fmt_bytes(diag.process_excess_storage_read_bytes),
        ),
        (
            "Proc>Dev Write".to_string(),
            fmt_bytes(diag.process_excess_storage_write_bytes),
        ),
    ];
    if let Some(backend) = &diag.trace_backend {
        rows.push(("Trace Backend".to_string(), backend.clone()));
        rows.push((
            "Trace Events".to_string(),
            diag.trace_event_count.unwrap_or(0).to_string(),
        ));
    }
    rows
}

fn detect_hostname() -> String {
    if let Some(host) = env::var("HOSTNAME")
        .ok()
        .filter(|value| !value.trim().is_empty())
    {
        return host;
    }
    let mut buffer = [0 as libc::c_char; 256];
    let rc = unsafe { libc::gethostname(buffer.as_mut_ptr(), buffer.len()) };
    if rc == 0 {
        let host = unsafe { CStr::from_ptr(buffer.as_ptr()) }
            .to_string_lossy()
            .trim()
            .to_string();
        if !host.is_empty() {
            return host;
        }
    }
    "-".to_string()
}

#[cfg(test)]
mod tests {
    use super::{
        abbreviate_for_display, resolve_color_enabled, set_color_policy, shrink_widths_to_budget,
        truncate_text, visible_width, Align, ColorPolicy, Column, Ui,
    };

    #[test]
    fn truncates_with_unicode_ellipsis() {
        assert_eq!(truncate_text("abcdefgh", 6), "abcde…");
    }

    #[test]
    fn abbreviates_long_paths_to_preserve_filename() {
        assert_eq!(
            abbreviate_for_display("/home/nima/.local/share/emmc-lab/session.json", 25),
            "/home/nima/…/session.json"
        );
    }

    #[test]
    fn truncates_path_in_middle_when_column_is_narrow() {
        assert_eq!(
            truncate_text("/home/nima/.local/share/session.json", 14),
            "/home/…on.json"
        );
    }

    #[test]
    fn fits_table_within_requested_width() {
        let ui = Ui {
            width: 72,
            height: 40,
            color: false,
            host: "-".to_string(),
            runtime_env: "local".to_string(),
        };
        let rendered = ui.table(
            "Test",
            &[
                Column {
                    header: "A",
                    min: 4,
                    max: 20,
                    align: Align::Left,
                },
                Column {
                    header: "B",
                    min: 4,
                    max: 20,
                    align: Align::Left,
                },
                Column {
                    header: "C",
                    min: 4,
                    max: 20,
                    align: Align::Left,
                },
            ],
            &[vec![
                "abcdefghijklmnopqrstuvwxyz".to_string(),
                "abcdefghijklmnopqrstuvwxyz".to_string(),
                "abcdefghijklmnopqrstuvwxyz".to_string(),
            ]],
            None,
        );
        let widest = rendered.lines().map(visible_width).max().unwrap_or(0);
        assert!(widest <= 72);
    }

    #[test]
    fn computes_visible_width_without_ansi_sequences() {
        assert_eq!(visible_width("\u{1b}[1;36mhello\u{1b}[0m"), 5);
    }

    #[test]
    fn color_policy_respects_no_color_overrides() {
        set_color_policy(ColorPolicy::Always);
        assert!(resolve_color_enabled(
            ColorPolicy::Always,
            true,
            "xterm-256color",
            false
        ));
        set_color_policy(ColorPolicy::Never);
        assert!(!resolve_color_enabled(
            ColorPolicy::Never,
            false,
            "xterm-256color",
            true
        ));
        set_color_policy(ColorPolicy::Auto);
        assert!(!resolve_color_enabled(
            ColorPolicy::Auto,
            false,
            "dumb",
            true
        ));
        assert!(!resolve_color_enabled(
            ColorPolicy::Auto,
            true,
            "xterm-256color",
            true
        ));
        assert!(resolve_color_enabled(
            ColorPolicy::Auto,
            false,
            "xterm-256color",
            true
        ));
    }

    #[test]
    fn shrinks_widths_below_nominal_minimum_when_terminal_is_narrow() {
        let mut widths = vec![20, 20, 20, 20];
        let desired = widths.clone();
        let minimum = vec![8, 8, 8, 8];
        shrink_widths_to_budget(&mut widths, &desired, &minimum, 20);
        assert_eq!(widths.iter().sum::<usize>(), 20);
        assert!(widths.iter().all(|width| *width >= 1));
    }

    #[test]
    fn joins_panels_horizontally_when_they_fit() {
        let ui = Ui {
            width: 120,
            height: 40,
            color: false,
            host: "-".to_string(),
            runtime_env: "local".to_string(),
        };
        let joined = ui.pair(
            |ui| {
                ui.kv_table(
                    "Left",
                    &[("A".to_string(), "1".to_string())],
                    Some(ui.width),
                )
            },
            |ui| {
                ui.kv_table(
                    "Right",
                    &[("B".to_string(), "2".to_string())],
                    Some(ui.width),
                )
            },
        );
        assert!(joined.contains("Left"));
        assert!(joined.contains("Right"));
        assert!(joined
            .lines()
            .any(|line| line.contains("Left") && line.contains("Right")));
    }
}
