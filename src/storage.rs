use crate::diagnostics::DiagnosticReport;
use crate::engine::{IntervalStats, RunSummary};
use crate::health::EmmcHealthSnapshot;
use crate::profile::Profile;
use crate::system::{collect_system_snapshot, AppPaths, SystemSnapshot};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionRecord {
    pub session_id: String,
    pub created_at: DateTime<Utc>,
    pub system: SystemSnapshot,
    pub profile: Option<Profile>,
    pub run_summary: Option<RunSummary>,
    pub interval_stats: Vec<IntervalStats>,
    pub health_before: Option<EmmcHealthSnapshot>,
    pub health_after: Option<EmmcHealthSnapshot>,
    pub diagnostics: Option<DiagnosticReport>,
    pub notes: Vec<String>,
    pub contamination_note: Option<String>,
}

impl SessionRecord {
    pub fn new(session_id: String) -> Self {
        Self {
            session_id,
            created_at: Utc::now(),
            system: collect_system_snapshot(),
            profile: None,
            run_summary: None,
            interval_stats: Vec::new(),
            health_before: None,
            health_after: None,
            diagnostics: None,
            notes: Vec::new(),
            contamination_note: None,
        }
    }
}

pub fn session_dir(paths: &AppPaths, session_id: &str) -> PathBuf {
    paths.sessions_dir.join(session_id)
}

pub fn save_session(paths: &AppPaths, record: &SessionRecord) -> Result<PathBuf> {
    let dir = session_dir(paths, &record.session_id);
    fs::create_dir_all(&dir).with_context(|| format!("failed to create {}", dir.display()))?;
    let summary_path = dir.join("session.json");
    let summary_text = serde_json::to_string_pretty(record)?;
    fs::write(&summary_path, summary_text)
        .with_context(|| format!("failed to write {}", summary_path.display()))?;

    let intervals_path = dir.join("intervals.jsonl");
    let mut body = String::new();
    for interval in &record.interval_stats {
        body.push_str(&serde_json::to_string(interval)?);
        body.push('\n');
    }
    fs::write(&intervals_path, body)
        .with_context(|| format!("failed to write {}", intervals_path.display()))?;

    #[cfg(feature = "sqlite")]
    persist_sqlite(paths, record)?;

    Ok(summary_path)
}

pub fn load_session(paths: &AppPaths, session_id: &str) -> Result<SessionRecord> {
    let path = session_dir(paths, session_id).join("session.json");
    let text = fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let record = serde_json::from_str(&text)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    Ok(record)
}

pub fn list_sessions(paths: &AppPaths) -> Result<Vec<String>> {
    let mut ids = Vec::new();
    for entry in fs::read_dir(&paths.sessions_dir)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            ids.push(entry.file_name().to_string_lossy().to_string());
        }
    }
    ids.sort();
    ids.reverse();
    Ok(ids)
}

pub fn list_profiles(paths: &AppPaths) -> Result<Vec<PathBuf>> {
    let mut profiles = Vec::new();
    if !paths.profiles_dir.exists() {
        return Ok(profiles);
    }
    for entry in fs::read_dir(&paths.profiles_dir)? {
        let entry = entry?;
        if entry.file_type()?.is_file() {
            let path = entry.path();
            if matches!(path.extension().and_then(|e| e.to_str()), Some("yaml" | "yml")) {
                profiles.push(path);
            }
        }
    }
    profiles.sort();
    Ok(profiles)
}

pub fn infer_profile_path(paths: &AppPaths, name: &str) -> PathBuf {
    let sanitized = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect::<String>();
    paths.profiles_dir.join(format!("{}.yaml", sanitized))
}

#[cfg(feature = "sqlite")]
fn persist_sqlite(paths: &AppPaths, record: &SessionRecord) -> Result<()> {
    use rusqlite::{params, Connection};
    let db_path = paths.base_dir.join("emmc-lab.sqlite3");
    let conn = Connection::open(db_path)?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS sessions (
            session_id TEXT PRIMARY KEY,
            created_at TEXT NOT NULL,
            mode TEXT,
            summary_json TEXT NOT NULL
        );",
    )?;
    let mode = if record.run_summary.is_some() {
        "run"
    } else if record.diagnostics.is_some() {
        "diagnostic"
    } else {
        "other"
    };
    conn.execute(
        "INSERT OR REPLACE INTO sessions (session_id, created_at, mode, summary_json) VALUES (?1, ?2, ?3, ?4)",
        params![
            record.session_id,
            record.created_at.to_rfc3339(),
            mode,
            serde_json::to_string(record)?
        ],
    )?;
    Ok(())
}

#[allow(dead_code)]
pub fn load_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T> {
    let text = fs::read_to_string(path)?;
    Ok(serde_json::from_str(&text)?)
}
