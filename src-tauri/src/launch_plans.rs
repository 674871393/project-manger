use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LaunchPlan {
    id: String,
    name: String,
    project_ids: Vec<String>,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LaunchPlanConfig {
    plans: Vec<LaunchPlan>,
    last_used_id: Option<String>,
}

fn plan_file(app: &AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_data_dir()
        .map(|dir| dir.join("launch-plans.json"))
        .map_err(|error| format!("获取启动方案目录失败: {error}"))
}

#[tauri::command]
pub(crate) fn load_launch_plans(app: AppHandle) -> Result<LaunchPlanConfig, String> {
    match fs::read(plan_file(&app)?) {
        Ok(data) => {
            serde_json::from_slice(&data).map_err(|error| format!("启动方案格式错误: {error}"))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(LaunchPlanConfig::default())
        }
        Err(error) => Err(format!("加载启动方案失败: {error}")),
    }
}

#[tauri::command]
pub(crate) fn save_launch_plans(app: AppHandle, config: LaunchPlanConfig) -> Result<(), String> {
    let path = plan_file(&app)?;
    let data =
        serde_json::to_vec_pretty(&config).map_err(|error| format!("生成启动方案失败: {error}"))?;
    crate::commands::save_local_file(&path, &data)
        .map_err(|error| format!("保存启动方案失败: {error}"))
}
