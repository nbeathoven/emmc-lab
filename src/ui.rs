use crate::health::HealthReport;
use crate::storage::SessionRecord;
use crate::system::{AppPaths, CapabilityReport, DeviceInfo, DoctorReport, SystemSnapshot};
use std::cmp::min;
use std::cmp::Reverse;
use std::env;
use std::fmt::Write as _;

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

struct Ui {
    width: usize,
    height: usize,
    color: bool,
}

impl Ui {
    fn detect() -> Self {
        let width = stdout_width()
            .or_else(|| {
                env::var("COLUMNS")
                    .ok()
                    .and_then(|value| value.parse::<usize>().ok())
            })
            .unwrap_or(100)
            .clamp(72, 160);
        let height = stdout_height()
            .or_else(|| {
                env::var("LINES")
                    .ok()
                    .and_then(|value| value.parse::<usize>().ok())
            })
            .unwrap_or(40)
            .clamp(20, 80);
        let no_color = env::var_os("NO_COLOR").is_some();
        let term = env::var("TERM").unwrap_or_default();
        let color = !no_color
            && !term.is_empty()
            && term != "dumb"
            && unsafe { libc::isatty(libc::STDOUT_FILENO) == 1 };
        Self {
            width,
            height,
            color,
        }
    }

    fn banner(&self, title: &str) -> String {
        let mut out = String::new();
        let line = "=".repeat(min(self.width, title.len().max(12)));
        let _ = writeln!(&mut out, "{}", self.paint(title, "1;36"));
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

    fn flow(&self, sections: &[String]) -> String {
        sections
            .iter()
            .filter(|section| !section.is_empty())
            .cloned()
            .collect::<Vec<_>>()
            .join("")
    }

    fn pair(&self, left: String, right: String) -> String {
        self.side_by_side(&left, &right)
            .unwrap_or_else(|| self.flow(&[left, right]))
    }

    fn kv_table(&self, title: &str, rows: &[(String, String)]) -> String {
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
        self.table(title, &columns, &table_rows)
    }

    fn table(&self, title: &str, columns: &[Column<'_>], rows: &[Vec<String>]) -> String {
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

        self.fit_width(columns, &mut widths);
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

    fn fit_width(&self, columns: &[Column<'_>], widths: &mut [usize]) {
        let mut total = self.table_width(widths.len(), widths);
        while total > self.width {
            let Some((index, _)) = widths
                .iter()
                .enumerate()
                .filter(|(index, width)| **width > columns[*index].min)
                .max_by_key(|(_, width)| **width)
            else {
                break;
            };
            widths[index] -= 1;
            total -= 1;
        }
    }

    fn table_width(&self, column_count: usize, widths: &[usize]) -> usize {
        widths.iter().sum::<usize>() + (column_count * 3) + 1
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
        let sanitized = sanitize_inline(value);
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

pub fn render_session_summary(record: &SessionRecord) -> String {
    let ui = Ui::detect();
    let mut out = ui.banner("EMMC-LAB SESSION SUMMARY");
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
    let session_panel = ui.kv_table("Session", &session_rows);

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
        let run_meta_panel = ui.kv_table("Run Metadata", &run_rows);
        out.push_str(&ui.pair(session_panel, run_meta_panel));

        let run_table = vec![
            vec![
                "Completed I/O".to_string(),
                run.read_ios_completed.to_string(),
                run.write_ios_completed.to_string(),
            ],
            vec![
                "Completed bytes".to_string(),
                fmt_bytes(run.read_bytes_completed),
                fmt_bytes(run.write_bytes_completed),
            ],
            vec![
                "Failed I/O".to_string(),
                run.failed_reads.to_string(),
                run.failed_writes.to_string(),
            ],
            vec![
                "Partial I/O".to_string(),
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
        let run_perf_panel = ui.table(
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
        );

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
        let reliability_panel = ui.kv_table("Reliability", &reliability_rows);
        out.push_str(&ui.pair(run_perf_panel, reliability_panel));
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
        let diag_rows = vec![
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
        ];
        out.push_str(&ui.pair(session_panel, ui.kv_table("Diagnostics", &diag_rows)));
    } else {
        out.push_str(&session_panel);
    }

    if let Some(diag) = &record.diagnostics {
        if record.run_summary.is_some() {
            let diag_rows = vec![
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
            ];
            out.push_str(&ui.kv_table("Diagnostics", &diag_rows));
        }
        out.push_str(&ui.note(
            "Attribution:",
            "Logical syscall bytes and storage-layer bytes are reported separately.",
        ));
        out.push_str(&ui.note("Detail:", &diag.attribution_note));
        if let Some(message) = &diag.fallback_message {
            out.push_str(&ui.note("Fallback:", message));
        }

        let process_limit = row_limit(ui.width);
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
            let processes_panel = ui.table(
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
            );

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
            let path_panel = ui.table(
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
            );
            out.push_str(&ui.pair(processes_panel, path_panel));
        }

        let file_rows = diag
            .top_files
            .iter()
            .take(row_limit(ui.width))
            .map(|file| {
                vec![
                    file.file_path.clone(),
                    file.last_pid_touching_file.to_string(),
                    file.readers_count.to_string(),
                    file.writers_count.to_string(),
                    file.read_ops.to_string(),
                    file.write_ops.to_string(),
                    fmt_bytes(file.read_bytes),
                    fmt_bytes(file.write_bytes),
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
                        header: "Read Ops",
                        min: 8,
                        max: 9,
                        align: Align::Right,
                    },
                    Column {
                        header: "Write Ops",
                        min: 9,
                        max: 10,
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
                ],
                &file_rows,
            ));
        }

        let directory_rows = diag
            .top_directories
            .iter()
            .take(row_limit(ui.width))
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
            ));
        }

        let mut sorted_devices = diag.device_totals.clone();
        sorted_devices.sort_by_key(|device| diagnostic_device_rank(&device.device, device));
        let device_rows = sorted_devices
            .iter()
            .take(row_limit(ui.width))
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
            ));
        }
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
        out.push_str(&ui.kv_table("Health Snapshots", &health_rows));
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

pub fn render_health_report(report: &HealthReport) -> String {
    let ui = Ui::detect();
    let mut out = ui.banner("EMMC-LAB HEALTH");
    let system_rows = system_rows(&report.system);
    let system_panel = ui.kv_table("System", &system_rows);
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
    ];
    let health_panel = ui.kv_table("eMMC Health", &emmc_rows);
    out.push_str(&ui.pair(system_panel, health_panel));
    out.push_str(&ui.note("Note:", &report.emmc.note));
    if let Some(raw) = &report.emmc.raw_text {
        out.push_str(&ui.note(
            "Raw mmc-utils:",
            &truncate_text(raw, ui.width.saturating_sub(20)),
        ));
    }
    out.push_str(&ui.kv_table("Capabilities", &capability_rows(&report.capabilities)));
    out
}

pub fn render_live_monitor(
    report: &crate::diagnostics::DiagnosticReport,
    interval_ms: u64,
) -> String {
    let ui = Ui::detect();
    let mut out = ui.banner("EMMC-LAB LIVE MONITOR");
    let compact = ui.height <= 28;
    let medium = ui.height <= 40;
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
        ("Controls".to_string(), "q quit".to_string()),
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
    ];
    out.push_str(&ui.pair(
        ui.kv_table("Monitor", &overview_rows),
        ui.kv_table("Notes", &note_rows),
    ));

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
        )
    };

    if compact {
        out.push_str(&process_panel);
        out.push_str(&ui.note(
            "Compact:",
            "lower sections hidden on short screens; use a taller terminal or timed capture for full detail",
        ));
        out.push_str(&ui.note(
            "Attribution:",
            "best-effort procfs monitor; logical and storage bytes remain separate",
        ));
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
        )
    };
    if !process_panel.is_empty() || !device_panel.is_empty() {
        out.push_str(&ui.pair(process_panel, device_panel));
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
                ],
                &file_rows,
            ));
        }
    }

    out.push_str(&ui.note(
        "Attribution:",
        "best-effort procfs monitor; logical and storage bytes remain separate",
    ));
    out
}

pub fn render_doctor_report(report: &DoctorReport) -> String {
    let ui = Ui::detect();
    let mut out = ui.banner("EMMC-LAB DOCTOR");
    let generated = vec![("Generated".to_string(), report.generated_at.to_rfc3339())];
    let run_panel = ui.kv_table("Run", &generated);
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
    let capabilities_panel = ui.kv_table("Capabilities", &capability_rows(&report.capabilities));
    out.push_str(&ui.pair(run_panel, capabilities_panel));
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
    ));

    let visible_devices = primary_devices(&report.devices);
    let hidden_count = report.devices.len().saturating_sub(visible_devices.len());
    let device_rows = visible_devices
        .iter()
        .take(row_limit(ui.width) + 2)
        .map(device_row)
        .collect::<Vec<_>>();
    if !device_rows.is_empty() {
        out.push_str(&ui.table("Primary Devices", &device_columns(), &device_rows));
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

pub fn render_settings(
    paths: &AppPaths,
    system: &SystemSnapshot,
    caps: &CapabilityReport,
) -> String {
    let ui = Ui::detect();
    let mut out = ui.banner("EMMC-LAB SETTINGS");
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
    let paths_panel = ui.kv_table("Paths", &path_rows);
    let system_panel = ui.kv_table("System", &system_rows(system));
    out.push_str(&ui.pair(paths_panel, system_panel));
    out.push_str(&ui.kv_table("Capabilities", &capability_rows(caps)));
    out
}

pub fn render_device_list(devices: &[DeviceInfo]) -> String {
    let ui = Ui::detect();
    let mut out = ui.banner("EMMC-LAB DEVICES");
    let rows = devices.iter().map(device_row).collect::<Vec<_>>();
    out.push_str(&ui.table("Visible Devices", &device_columns(), &rows));
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

fn row_limit(width: usize) -> usize {
    if width < 90 {
        3
    } else if width < 120 {
        5
    } else {
        7
    }
}

fn live_process_limit(width: usize, height: usize) -> usize {
    let width_limit = row_limit(width);
    if height <= 28 {
        width_limit.min(4)
    } else if height <= 40 {
        width_limit.min(5)
    } else {
        width_limit
    }
}

fn live_device_limit(width: usize, height: usize) -> usize {
    let width_limit = row_limit(width);
    if height <= 32 {
        width_limit.min(3)
    } else {
        width_limit
    }
}

fn live_file_limit(width: usize, height: usize) -> usize {
    let width_limit = row_limit(width);
    if height <= 56 {
        width_limit.min(4)
    } else {
        width_limit
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

fn truncate_text(value: &str, width: usize) -> String {
    let len = value.chars().count();
    if len <= width {
        return value.to_string();
    }
    if width <= 3 {
        return value.chars().take(width).collect();
    }
    let mut out = value.chars().take(width - 3).collect::<String>();
    out.push_str("...");
    out
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

#[cfg(test)]
mod tests {
    use super::{truncate_text, visible_width, Align, Column, Ui};

    #[test]
    fn truncates_with_ascii_ellipsis() {
        assert_eq!(truncate_text("abcdefgh", 6), "abc...");
    }

    #[test]
    fn fits_table_within_requested_width() {
        let ui = Ui {
            width: 72,
            height: 40,
            color: false,
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
        );
        let widest = rendered.lines().map(str::len).max().unwrap_or(0);
        assert!(widest <= 72);
    }

    #[test]
    fn computes_visible_width_without_ansi_sequences() {
        assert_eq!(visible_width("\u{1b}[1;36mhello\u{1b}[0m"), 5);
    }

    #[test]
    fn joins_panels_horizontally_when_they_fit() {
        let ui = Ui {
            width: 120,
            height: 40,
            color: false,
        };
        let left = ui.kv_table("Left", &[("A".to_string(), "1".to_string())]);
        let right = ui.kv_table("Right", &[("B".to_string(), "2".to_string())]);
        let joined = ui.pair(left.clone(), right.clone());
        assert!(joined.contains("Left"));
        assert!(joined.contains("Right"));
        assert!(!joined.starts_with(&format!("{}{}", left, right)));
    }
}
