// 应用设置（PRD FR-7 全局设置的落地）
//
// 存 appDataDir/settings.json，两项：
// - minimize_on_close：点窗口 X 是退出（默认）还是最小化到托盘
// - autostart 由 tauri-plugin-autostart 管理（写注册表/启动项），
//   前端开关时直接调插件的 enable/disable，这里不重复存

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};
use tauri_plugin_autostart::ManagerExt;

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct AppSettings {
    /// 点 X 最小化到托盘而不是退出（默认 false = 直接退出）
    pub minimize_on_close: bool,
    /// 开机自启动（镜像自 autostart 插件状态，便于前端初始化 UI）
    pub autostart: bool,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            minimize_on_close: false,
            autostart: false,
        }
    }
}

fn settings_path(app: &AppHandle) -> Option<std::path::PathBuf> {
    app.path()
        .app_data_dir()
        .ok()
        .map(|dir| dir.join("settings.json"))
}

/// 读取设置；文件不存在/损坏时返回默认值（首次启动场景）
pub fn load_settings_inner(app: &AppHandle) -> Result<AppSettings, String> {
    let Some(path) = settings_path(app) else {
        return Ok(AppSettings::default());
    };
    match std::fs::read_to_string(&path) {
        Ok(content) => serde_json::from_str(&content).map_err(|e| e.to_string()),
        Err(_) => Ok(AppSettings::default()),
    }
}

#[tauri::command]
pub fn load_settings(app: AppHandle) -> Result<AppSettings, String> {
    let mut settings = load_settings_inner(&app)?;
    // autostart 以插件实际状态为准（用户可能在外部改过启动项）
    settings.autostart = app.autolaunch().is_enabled().unwrap_or(false);
    Ok(settings)
}

#[tauri::command]
pub fn save_settings(
    app: AppHandle,
    settings: AppSettings,
) -> Result<(), String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let json = serde_json::to_string_pretty(&settings).map_err(|e| e.to_string())?;
    std::fs::write(dir.join("settings.json"), json).map_err(|e| e.to_string())?;

    // 开机自启联动插件（写注册表启动项）。
    // 只在状态确实需要变化时才写：disable 一个从未 enable 过的条目
    // 会因注册表值不存在报 os error 2，enable 已启用的也是无谓重复。
    let autolaunch = app.autolaunch();
    let enabled = autolaunch.is_enabled().unwrap_or(false);
    if settings.autostart && !enabled {
        autolaunch.enable().map_err(|e| e.to_string())?;
    } else if !settings.autostart && enabled {
        autolaunch.disable().map_err(|e| e.to_string())?;
    }
    Ok(())
}
