use crate::health::{collect_health, EmmcHealthSnapshot};
use crate::lba_trace::{match_lba_to_file, LbaTraceReport};
use crate::system::AppPaths;
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FaDeviceId {
    pub mid: String,
    pub pnm: String,
    pub fw_version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FaExtCsdSnapshot {
    pub life_time_est_a: u8,
    pub life_time_est_b: u8,
    pub pre_eol_info: u8,
    pub bkops_status: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FaBadBlockRange {
    pub start_lba: u64,
    pub end_lba: u64,
    pub failure_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FaReport {
    pub schema_version: String,
    pub device_id: FaDeviceId,
    pub ext_csd_snapshot: FaExtCsdSnapshot,
    pub bad_block_ranges: Vec<FaBadBlockRange>,
    pub notes: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompareNote {
    pub field: String,
    pub local_value: String,
    pub fa_value: String,
    pub status: String,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorrelationHit {
    pub lba_range: (u64, u64),
    pub failure_type: String,
    pub matched_file: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorrelationSummary {
    pub overlapping_ranges: Vec<CorrelationHit>,
    pub total_overlapping_lbas: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthCompareReport {
    pub local_snapshot: EmmcHealthSnapshot,
    pub fa_report: FaReport,
    pub field_comparisons: Vec<CompareNote>,
    pub correlation: Option<CorrelationSummary>,
}

/// Compare the current live EXT_CSD view against a failure-analysis JSON report.
pub fn compare_health(
    paths: &AppPaths,
    device: &Path,
    fa_report_path: &Path,
    trace_report: Option<&LbaTraceReport>,
    block_size_bytes: Option<u64>,
) -> Result<HealthCompareReport> {
    // Keep the live read and the FA file load together here so the caller gets one
    // comparison object built from a single point-in-time snapshot.
    let local = collect_health(paths, device, None, None)?.emmc;
    let fa_report = load_fa_report(fa_report_path)?;
    let field_comparisons = compare_extcsd_fields(&local, &fa_report);

    // Correlation is optional because some users only have the FA export and live
    // EXT_CSD state, not a matching LBA trace capture.
    let correlation = trace_report.and_then(|trace| {
        let block_size_bytes = block_size_bytes.or(local.erase_group_size_bytes)?;
        Some(correlate_block_ranges(trace, &fa_report, block_size_bytes))
    });
    Ok(HealthCompareReport {
        local_snapshot: local,
        fa_report,
        field_comparisons,
        correlation,
    })
}

/// Load and validate the FA JSON schema.
fn load_fa_report(path: &Path) -> Result<FaReport> {
    let text =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let report: FaReport = serde_json::from_str(&text)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    if report.schema_version != "1.0" {
        return Err(anyhow!(
            "unsupported FA schema_version {}; expected 1.0",
            report.schema_version
        ));
    }
    Ok(report)
}

/// Compare overlapping health fields and classify drift.
fn compare_extcsd_fields(local: &EmmcHealthSnapshot, fa: &FaReport) -> Vec<CompareNote> {
    vec![
        compare_numeric_field(
            "life_time_est_a",
            parse_hexish_u8(local.device_life_time_est_typ_a.as_deref()),
            Some(fa.ext_csd_snapshot.life_time_est_a),
        ),
        compare_numeric_field(
            "life_time_est_b",
            parse_hexish_u8(local.device_life_time_est_typ_b.as_deref()),
            Some(fa.ext_csd_snapshot.life_time_est_b),
        ),
        compare_numeric_field(
            "pre_eol_info",
            parse_hexish_u8(local.pre_eol_info.as_deref()),
            Some(fa.ext_csd_snapshot.pre_eol_info),
        ),
        compare_numeric_field(
            "bkops_status",
            local.bkops_status,
            Some(fa.ext_csd_snapshot.bkops_status),
        ),
    ]
}

fn compare_numeric_field(field: &str, local: Option<u8>, fa: Option<u8>) -> CompareNote {
    let local_value = local
        .map(|value| format!("0x{value:02x}"))
        .unwrap_or_else(|| "unavailable".to_string());
    let fa_value = fa
        .map(|value| format!("0x{value:02x}"))
        .unwrap_or_else(|| "unavailable".to_string());
    let status = match (local, fa) {
        (Some(local), Some(fa)) if local == fa => "match",
        (Some(local), Some(fa)) if local > fa => "worse",
        (Some(_), Some(_)) => "drift",
        _ => "mismatch",
    };
    let detail = match status {
        "match" => "live device agrees with the FA snapshot".to_string(),
        "worse" => {
            "live value is higher than the FA snapshot and indicates additional wear or pressure"
                .to_string()
        }
        "drift" => "live value differs from the FA snapshot but is not higher".to_string(),
        _ => "one side did not provide a comparable value".to_string(),
    };
    CompareNote {
        field: field.to_string(),
        local_value,
        fa_value,
        status: status.to_string(),
        detail,
    }
}

fn parse_hexish_u8(value: Option<&str>) -> Option<u8> {
    let value = value?.trim();
    let value = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
        .unwrap_or(value);
    u8::from_str_radix(value, 16)
        .ok()
        .or_else(|| value.parse::<u8>().ok())
}

/// Correlate FA bad-block LBA ranges with observed trace traffic.
fn correlate_block_ranges(
    trace: &LbaTraceReport,
    fa: &FaReport,
    _block_size_bytes: u64,
) -> CorrelationSummary {
    let mut overlapping_ranges = Vec::new();
    let mut total_overlapping_lbas = 0_u64;
    for bad_range in &fa.bad_block_ranges {
        let mut matched = false;
        for event in &trace.events {
            let event_end = event.lba.saturating_add(event.length_sectors);
            let overlap_start = event.lba.max(bad_range.start_lba);
            let overlap_end = event_end.min(bad_range.end_lba);
            if overlap_end <= overlap_start {
                continue;
            }
            matched = true;
            total_overlapping_lbas =
                total_overlapping_lbas.saturating_add(overlap_end.saturating_sub(overlap_start));
            overlapping_ranges.push(CorrelationHit {
                lba_range: (overlap_start, overlap_end),
                failure_type: bad_range.failure_type.clone(),
                matched_file: match_lba_to_file(overlap_start, &trace.owner_index)
                    .map(|hit| hit.file.display().to_string()),
            });
        }
        if !matched {
            overlapping_ranges.push(CorrelationHit {
                lba_range: (bad_range.start_lba, bad_range.end_lba),
                failure_type: bad_range.failure_type.clone(),
                matched_file: None,
            });
        }
    }
    CorrelationSummary {
        overlapping_ranges,
        total_overlapping_lbas,
    }
}

/// Render a standalone HTML comparison report with inline CSS.
pub fn render_health_compare_html(report: &HealthCompareReport) -> String {
    let mut html = String::new();
    html.push_str("<!doctype html><html><head><meta charset=\"utf-8\"><title>emmc-lab health compare</title><style>body{font-family:sans-serif;margin:2rem;background:#faf7f2;color:#222}table{border-collapse:collapse;width:100%;margin:1rem 0}th,td{border:1px solid #d6c9be;padding:0.5rem;text-align:left}th{background:#efe3d8}code{background:#f0ece8;padding:0.1rem 0.3rem;border-radius:4px}.match{color:#1d6f42}.worse{color:#a43f14}.drift{color:#885d00}.mismatch{color:#7a1b1b}</style></head><body>");
    html.push_str("<h1>eMMC Health Comparison</h1>");
    html.push_str(&format!(
        "<p>Local device: <code>{}</code></p>",
        report.local_snapshot.device.display()
    ));
    html.push_str("<h2>Field Comparison</h2><table><tr><th>Field</th><th>Local</th><th>FA</th><th>Status</th><th>Detail</th></tr>");
    for note in &report.field_comparisons {
        html.push_str(&format!(
            "<tr><td>{}</td><td>{}</td><td>{}</td><td class=\"{}\">{}</td><td>{}</td></tr>",
            note.field, note.local_value, note.fa_value, note.status, note.status, note.detail
        ));
    }
    html.push_str("</table>");
    if let Some(correlation) = &report.correlation {
        html.push_str("<h2>Correlation</h2><table><tr><th>LBA Range</th><th>Failure</th><th>Matched File</th></tr>");
        for hit in &correlation.overlapping_ranges {
            html.push_str(&format!(
                "<tr><td>{}-{} </td><td>{}</td><td>{}</td></tr>",
                hit.lba_range.0,
                hit.lba_range.1,
                hit.failure_type,
                hit.matched_file.as_deref().unwrap_or("-")
            ));
        }
        html.push_str("</table>");
    }
    html.push_str("</body></html>");
    html
}

#[cfg(test)]
mod tests {
    use super::{
        compare_extcsd_fields, load_fa_report, render_health_compare_html, CompareNote,
        CorrelationSummary, FaBadBlockRange, FaDeviceId, FaExtCsdSnapshot, FaReport,
        HealthCompareReport,
    };
    use crate::health::EmmcHealthSnapshot;
    use chrono::Utc;
    use std::fs;
    use std::path::PathBuf;
    use tempfile::tempdir;

    #[test]
    fn rejects_unknown_schema_versions() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("fa.json");
        fs::write(
            &path,
            r#"{"schema_version":"2.0","device_id":{"mid":"0x15","pnm":"A","fw_version":"1"},"ext_csd_snapshot":{"life_time_est_a":1,"life_time_est_b":1,"pre_eol_info":1,"bkops_status":0},"bad_block_ranges":[],"notes":""}"#,
        )
        .expect("write schema");
        assert!(load_fa_report(&path).is_err());
    }

    #[test]
    fn loads_valid_schema_and_compares_fields() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("fa.json");
        fs::write(
            &path,
            r#"{"schema_version":"1.0","device_id":{"mid":"0x15","pnm":"AJTD4R","fw_version":"0300"},"ext_csd_snapshot":{"life_time_est_a":3,"life_time_est_b":2,"pre_eol_info":1,"bkops_status":0},"bad_block_ranges":[],"notes":"ok"}"#,
        )
        .expect("write schema");
        let report = load_fa_report(&path).expect("valid report");
        assert_eq!(report.device_id.pnm, "AJTD4R");

        let local = EmmcHealthSnapshot {
            collected_at: Utc::now(),
            device: PathBuf::from("/dev/mmcblk0"),
            available: true,
            device_life_time_est_typ_a: Some("0x03".to_string()),
            device_life_time_est_typ_b: Some("0x04".to_string()),
            pre_eol_info: Some("0x01".to_string()),
            bkops_status: Some(1),
            note: String::new(),
            ..EmmcHealthSnapshot::default()
        };
        let notes = compare_extcsd_fields(&local, &report);
        assert_eq!(notes[0].status, "match");
        assert_eq!(notes[1].status, "worse");
        assert_eq!(notes[3].status, "worse");
    }

    #[test]
    fn html_report_renders_basic_sections() {
        let report = HealthCompareReport {
            local_snapshot: EmmcHealthSnapshot {
                collected_at: Utc::now(),
                device: PathBuf::from("/dev/mmcblk0"),
                available: true,
                note: String::new(),
                ..EmmcHealthSnapshot::default()
            },
            fa_report: FaReport {
                schema_version: "1.0".to_string(),
                device_id: FaDeviceId {
                    mid: "0x15".to_string(),
                    pnm: "AJTD4R".to_string(),
                    fw_version: "0300".to_string(),
                },
                ext_csd_snapshot: FaExtCsdSnapshot {
                    life_time_est_a: 1,
                    life_time_est_b: 1,
                    pre_eol_info: 1,
                    bkops_status: 0,
                },
                bad_block_ranges: vec![FaBadBlockRange {
                    start_lba: 10,
                    end_lba: 20,
                    failure_type: "read_ecc_uncorrectable".to_string(),
                }],
                notes: "notes".to_string(),
            },
            field_comparisons: vec![CompareNote {
                field: "pre_eol_info".to_string(),
                local_value: "0x01".to_string(),
                fa_value: "0x01".to_string(),
                status: "match".to_string(),
                detail: "same".to_string(),
            }],
            correlation: Some(CorrelationSummary {
                overlapping_ranges: Vec::new(),
                total_overlapping_lbas: 0,
            }),
        };
        let html = render_health_compare_html(&report);
        assert!(html.contains("Field Comparison"));
        assert!(html.contains("Correlation"));
    }
}
