use crate::storage::{load_session, SessionRecord};
use crate::system::AppPaths;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::fmt::Write as _;
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportFormat {
    Json,
    Csv,
    Html,
}

impl ExportFormat {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "json" => Some(Self::Json),
            "csv" => Some(Self::Csv),
            "html" => Some(Self::Html),
            _ => None,
        }
    }
}

pub fn terminal_summary(record: &SessionRecord) -> String {
    let mut out = String::new();
    let _ = writeln!(&mut out, "Session: {}", record.session_id);
    let _ = writeln!(&mut out, "Created: {}", record.created_at);
    if let Some(run) = &record.run_summary {
        let _ = writeln!(
            &mut out,
            "Run: backend={} target={} mode={}",
            run.backend,
            run.target_path.display(),
            run.target_mode
        );
        let _ = writeln!(
            &mut out,
            "Reads completed: {}  Writes completed: {}",
            run.read_ios_completed, run.write_ios_completed
        );
        let _ = writeln!(
            &mut out,
            "Read bytes: {}  Write bytes: {}",
            run.read_bytes_completed, run.write_bytes_completed
        );
        let _ = writeln!(
            &mut out,
            "Read latency p50/p95/p99/p99.9 (us): {}/{}/{}/{}",
            run.read_latency.p50_us,
            run.read_latency.p95_us,
            run.read_latency.p99_us,
            run.read_latency.p999_us
        );
        let _ = writeln!(
            &mut out,
            "Write latency p50/p95/p99/p99.9 (us): {}/{}/{}/{}",
            run.write_latency.p50_us,
            run.write_latency.p95_us,
            run.write_latency.p99_us,
            run.write_latency.p999_us
        );
        let _ = writeln!(
            &mut out,
            "Errors: failed_reads={} failed_writes={} partial_reads={} partial_writes={} retries={} verify_failures={} sync_failures={}",
            run.failed_reads,
            run.failed_writes,
            run.partial_reads,
            run.partial_writes,
            run.retries,
            run.verify_failures,
            run.sync_failures
        );
        let _ = writeln!(
            &mut out,
            "Logical range: start={} length={} logical_sector_size={}",
            run.addressed_range_start_bytes, run.addressed_range_length_bytes, run.logical_sector_size
        );
    }
    if let Some(diag) = &record.diagnostics {
        let _ = writeln!(
            &mut out,
            "Diagnostics: mode={:?} duration={}s unresolved_paths={} dropped_events={}",
            diag.mode, diag.duration_seconds, diag.unresolved_path_count, diag.dropped_event_count
        );
        if let Some(top) = diag.top_processes.first() {
            let _ = writeln!(
                &mut out,
                "Top process: pid={} cmd={} storage_r/s={:.1} storage_w/s={:.1}",
                top.pid, top.command, top.storage_read_bytes_per_sec, top.storage_write_bytes_per_sec
            );
        }
    }
    if let Some(note) = &record.contamination_note {
        let _ = writeln!(&mut out, "Interference note: {}", note);
    }
    if !record.notes.is_empty() {
        let _ = writeln!(&mut out, "Notes:");
        for note in &record.notes {
            let _ = writeln!(&mut out, "- {}", note);
        }
    }
    out
}

pub fn export_session(paths: &AppPaths, session_id: &str, format: ExportFormat) -> Result<Vec<PathBuf>> {
    let record = load_session(paths, session_id)?;
    fs::create_dir_all(&paths.exports_dir)?;
    match format {
        ExportFormat::Json => {
            let path = paths.exports_dir.join(format!("{}.json", session_id));
            fs::write(&path, serde_json::to_string_pretty(&record)?)?;
            Ok(vec![path])
        }
        ExportFormat::Html => {
            let path = paths.exports_dir.join(format!("{}.html", session_id));
            fs::write(&path, render_html(&record))?;
            Ok(vec![path])
        }
        ExportFormat::Csv => export_csv(paths, &record),
    }
}

fn export_csv(paths: &AppPaths, record: &SessionRecord) -> Result<Vec<PathBuf>> {
    let mut paths_out = Vec::new();

    let summary_path = paths.exports_dir.join(format!("{}_summary.csv", record.session_id));
    let mut summary_csv = String::from("session_id,created_at,target,backend,read_ios,write_ios,read_bytes,write_bytes,failed_reads,failed_writes,verify_failures,sync_failures\n");
    if let Some(run) = &record.run_summary {
        let line = format!(
            "{},{},{},{},{},{},{},{},{},{},{},{}\n",
            record.session_id,
            record.created_at.to_rfc3339(),
            csv_escape(&run.target_path.display().to_string()),
            run.backend,
            run.read_ios_completed,
            run.write_ios_completed,
            run.read_bytes_completed,
            run.write_bytes_completed,
            run.failed_reads,
            run.failed_writes,
            run.verify_failures,
            run.sync_failures,
        );
        summary_csv.push_str(&line);
    }
    fs::write(&summary_path, summary_csv)?;
    paths_out.push(summary_path);

    let interval_path = paths.exports_dir.join(format!("{}_intervals.csv", record.session_id));
    let mut interval_csv = String::from("second,read_ios_completed,write_ios_completed,read_bytes_completed,write_bytes_completed,failed_reads,failed_writes,partial_reads,partial_writes,retries,verify_failures,sync_failures\n");
    for i in &record.interval_stats {
        interval_csv.push_str(&format!(
            "{},{},{},{},{},{},{},{},{},{},{},{}\n",
            i.second,
            i.read_ios_completed,
            i.write_ios_completed,
            i.read_bytes_completed,
            i.write_bytes_completed,
            i.failed_reads,
            i.failed_writes,
            i.partial_reads,
            i.partial_writes,
            i.retries,
            i.verify_failures,
            i.sync_failures
        ));
    }
    fs::write(&interval_path, interval_csv)?;
    paths_out.push(interval_path);

    if let Some(diag) = &record.diagnostics {
        let process_path = paths.exports_dir.join(format!("{}_processes.csv", record.session_id));
        let mut process_csv = String::from("pid,user,command,exe_path,cwd,logical_read_bytes_per_sec,logical_write_bytes_per_sec,storage_read_bytes_per_sec,storage_write_bytes_per_sec,read_syscalls_per_sec,write_syscalls_per_sec,open_file_count,hottest_file,hottest_directory,observed_share_percent\n");
        for p in &diag.top_processes {
            process_csv.push_str(&format!(
                "{},{},{},{},{},{:.3},{:.3},{:.3},{:.3},{:.3},{:.3},{},{},{},{}\n",
                p.pid,
                csv_escape(&p.user),
                csv_escape(&p.command),
                csv_escape(&p.exe_path),
                csv_escape(&p.cwd),
                p.logical_read_bytes_per_sec,
                p.logical_write_bytes_per_sec,
                p.storage_read_bytes_per_sec,
                p.storage_write_bytes_per_sec,
                p.read_syscalls_per_sec,
                p.write_syscalls_per_sec,
                p.open_file_count,
                csv_escape(&p.hottest_file),
                csv_escape(&p.hottest_directory),
                p.observed_share_percent
            ));
        }
        fs::write(&process_path, process_csv)?;
        paths_out.push(process_path);
    }

    Ok(paths_out)
}

fn render_html(record: &SessionRecord) -> String {
    let mut html = String::new();
    html.push_str("<!doctype html><html><head><meta charset=\"utf-8\"><title>emmc-lab report</title><style>body{font-family:sans-serif;margin:2rem;color:#222;background:#faf7f2}table{border-collapse:collapse;width:100%;margin:1rem 0}th,td{border:1px solid #ccc;padding:0.5rem;text-align:left}code{background:#eee;padding:0.15rem 0.25rem}h1,h2{color:#8a3b12}</style></head><body>");
    html.push_str(&format!("<h1>emmc-lab Session {}</h1>", record.session_id));
    html.push_str("<p>Logical sector/LBA targeting only. This report does not claim fixed physical NAND cell targeting.</p>");
    html.push_str("<h2>Session Metadata</h2><table>");
    html.push_str(&format!(
        "<tr><th>Created</th><td>{}</td></tr>",
        record.created_at
    ));
    if let Some(run) = &record.run_summary {
        html.push_str(&format!(
            "<tr><th>Target</th><td><code>{}</code></td></tr>",
            run.target_path.display()
        ));
        html.push_str(&format!(
            "<tr><th>Backend</th><td>{}</td></tr>",
            run.backend
        ));
        html.push_str(&format!(
            "<tr><th>Logical Range</th><td>start={} length={} logical_sector_size={}</td></tr>",
            run.addressed_range_start_bytes, run.addressed_range_length_bytes, run.logical_sector_size
        ));
    }
    html.push_str("</table>");

    if let Some(run) = &record.run_summary {
        html.push_str("<h2>Performance Summary</h2><table>");
        html.push_str(&format!(
            "<tr><th>Completed I/O</th><td>reads={} writes={}</td></tr>",
            run.read_ios_completed, run.write_ios_completed
        ));
        html.push_str(&format!(
            "<tr><th>Completed Bytes</th><td>read={} write={}</td></tr>",
            run.read_bytes_completed, run.write_bytes_completed
        ));
        html.push_str(&format!(
            "<tr><th>Read Latency (us)</th><td>p50={} p95={} p99={} p99.9={}</td></tr>",
            run.read_latency.p50_us, run.read_latency.p95_us, run.read_latency.p99_us, run.read_latency.p999_us
        ));
        html.push_str(&format!(
            "<tr><th>Write Latency (us)</th><td>p50={} p95={} p99={} p99.9={}</td></tr>",
            run.write_latency.p50_us, run.write_latency.p95_us, run.write_latency.p99_us, run.write_latency.p999_us
        ));
        html.push_str(&format!(
            "<tr><th>Errors</th><td>failed_reads={} failed_writes={} partial_reads={} partial_writes={} retries={} verify_failures={} sync_failures={}</td></tr>",
            run.failed_reads,
            run.failed_writes,
            run.partial_reads,
            run.partial_writes,
            run.retries,
            run.verify_failures,
            run.sync_failures
        ));
        html.push_str("</table>");
    }

    if let Some(diag) = &record.diagnostics {
        html.push_str("<h2>Diagnostics</h2>");
        html.push_str(&format!("<p>{}</p>", diag.attribution_note));
        html.push_str("<table><tr><th>PID</th><th>User</th><th>Command</th><th>Logical Read B/s</th><th>Logical Write B/s</th><th>Storage Read B/s</th><th>Storage Write B/s</th><th>Hottest File</th><th>Hottest Directory</th></tr>");
        for p in diag.top_processes.iter().take(15) {
            html.push_str(&format!(
                "<tr><td>{}</td><td>{}</td><td>{}</td><td>{:.1}</td><td>{:.1}</td><td>{:.1}</td><td>{:.1}</td><td>{}</td><td>{}</td></tr>",
                p.pid,
                escape_html(&p.user),
                escape_html(&p.command),
                p.logical_read_bytes_per_sec,
                p.logical_write_bytes_per_sec,
                p.storage_read_bytes_per_sec,
                p.storage_write_bytes_per_sec,
                escape_html(&p.hottest_file),
                escape_html(&p.hottest_directory)
            ));
        }
        html.push_str("</table>");
    }

    if let Some(before) = &record.health_before {
        html.push_str("<h2>Health</h2><table>");
        html.push_str(&format!(
            "<tr><th>Before</th><td>available={} PRE_EOL_INFO={:?} LIFE_A={:?} LIFE_B={:?}</td></tr>",
            before.available,
            before.pre_eol_info,
            before.device_life_time_est_typ_a,
            before.device_life_time_est_typ_b
        ));
        if let Some(after) = &record.health_after {
            html.push_str(&format!(
                "<tr><th>After</th><td>available={} PRE_EOL_INFO={:?} LIFE_A={:?} LIFE_B={:?}</td></tr>",
                after.available,
                after.pre_eol_info,
                after.device_life_time_est_typ_a,
                after.device_life_time_est_typ_b
            ));
        }
        html.push_str("</table>");
    }

    if let Some(note) = &record.contamination_note {
        html.push_str(&format!("<p><strong>Interference note:</strong> {}</p>", escape_html(note)));
    }
    if !record.notes.is_empty() {
        html.push_str("<h2>Notes</h2><ul>");
        for note in &record.notes {
            html.push_str(&format!("<li>{}</li>", escape_html(note)));
        }
        html.push_str("</ul>");
    }
    html.push_str("</body></html>");
    html
}

fn csv_escape(value: &str) -> String {
    if value.contains(',') || value.contains('"') || value.contains('\n') {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
