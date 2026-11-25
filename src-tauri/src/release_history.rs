use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

use crate::commands::{read_log_tail_from, Project, ReleaseConfig};

const HISTORY_LIMIT: usize = 100;

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReleaseRecord {
    pub(crate) id: String,
    pub(crate) project_id: String,
    project_name: String,
    version: String,
    branch: String,
    pub(crate) commit: Option<String>,
    node_version: String,
    started_at: i64,
    pub(crate) duration_ms: u64,
    pub(crate) success: bool,
    pub(crate) zip_path: Option<String>,
    pub(crate) error: Option<String>,
}

impl ReleaseRecord {
    pub(crate) fn new(project: &Project, config: &ReleaseConfig, version: &str) -> Self {
        let now = chrono::Utc::now();
        Self {
            id: format!(
                "{}-{}",
                now.timestamp_nanos_opt().unwrap_or(now.timestamp_micros()),
                std::process::id()
            ),
            project_id: project.id.clone(),
            project_name: project.name.clone(),
            version: version.into(),
            branch: config.branch.clone(),
            commit: None,
            node_version: config.node_version.clone(),
            started_at: now.timestamp_millis(),
            duration_ms: 0,
            success: false,
            zip_path: None,
            error: None,
        }
    }
}

fn history_dir(app: &AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_data_dir()
        .map(|dir| dir.join("release-history"))
        .map_err(|e| format!("获取发布历史目录失败: {e}"))
}

pub(crate) fn create_history_log(app: &AppHandle, id: &str) -> Result<PathBuf, String> {
    let dir = history_dir(app)?;
    fs::create_dir_all(&dir).map_err(|e| format!("创建发布历史目录失败: {e}"))?;
    let log = dir.join(format!("{id}.log"));
    fs::File::create_new(&log).map_err(|e| format!("创建发布日志失败: {e}"))?;
    Ok(log)
}

fn records_in(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("读取发布历史失败: {e}")),
    };
    let mut paths: Vec<_> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    paths.sort_unstable_by(|a, b| b.cmp(a));
    Ok(paths)
}

pub(crate) fn save_record(app: &AppHandle, record: &ReleaseRecord) -> Result<(), String> {
    let dir = history_dir(app)?;
    let data = serde_json::to_vec_pretty(record).map_err(|e| format!("生成发布历史失败: {e}"))?;
    let path = dir.join(format!("{}.json", record.id));
    let temp = path.with_extension("json.tmp");
    fs::write(&temp, data)
        .and_then(|_| fs::rename(&temp, &path))
        .map_err(|e| format!("保存发布历史失败: {e}"))?;
    for old in records_in(&dir)?.into_iter().skip(HISTORY_LIMIT) {
        if fs::remove_file(&old).is_ok() {
            let _ = fs::remove_file(old.with_extension("log"));
        }
    }
    Ok(())
}

#[tauri::command]
pub(crate) fn list_release_history(app: AppHandle) -> Result<Vec<ReleaseRecord>, String> {
    records_in(&history_dir(&app)?)?
        .into_iter()
        .take(HISTORY_LIMIT)
        .map(|path| {
            let data = fs::read(&path).map_err(|e| format!("读取发布记录失败: {e}"))?;
            serde_json::from_slice(&data).map_err(|e| format!("发布记录格式错误: {e}"))
        })
        .collect()
}

#[tauri::command]
pub(crate) fn read_release_log(app: AppHandle, release_id: String) -> Result<Vec<String>, String> {
    let valid = release_id.split_once('-').is_some_and(|(time, pid)| {
        !time.is_empty()
            && !pid.is_empty()
            && time
                .bytes()
                .chain(pid.bytes())
                .all(|byte| byte.is_ascii_digit())
    });
    if !valid {
        return Err("无效的发布记录 ID".into());
    }
    read_log_tail_from(&history_dir(&app)?.join(format!("{release_id}.log")), 2000)
}
