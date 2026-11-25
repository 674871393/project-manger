// 应用设置（PRD FR-7 全局设置的落地）
//
// 存 appDataDir/settings.json，两项：
// - minimize_on_close：点窗口 X 是退出（默认）还是最小化到托盘
// - 正式构建通过 tauri-plugin-autostart 管理系统启动项。
// - 开发版依赖本地开发服务器，只保存窗口行为设置。

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

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsView {
    #[serde(flatten)]
    settings: AppSettings,
    autostart_available: bool,
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
pub fn load_settings(app: AppHandle) -> Result<SettingsView, String> {
    let mut settings = load_settings_inner(&app)?;
    let autostart_available = !tauri::is_dev();
    settings.autostart = if autostart_available {
        app.autolaunch().is_enabled().map_err(|e| e.to_string())?
    } else {
        false
    };
    Ok(SettingsView {
        settings,
        autostart_available,
    })
}

#[tauri::command]
pub fn save_settings(app: AppHandle, mut settings: AppSettings) -> Result<(), String> {
    let path = settings_path(&app).ok_or("无法定位设置文件")?;
    if tauri::is_dev() {
        settings.autostart = load_settings_inner(&app)?.autostart;
    } else {
        let autolaunch = app.autolaunch();
        if settings.autostart {
            // 重新登记当前正式版路径，修复移动旧版后留下的启动项。
            autolaunch.enable().map_err(|e| e.to_string())?;
        } else if autolaunch.is_enabled().map_err(|e| e.to_string())? {
            autolaunch.disable().map_err(|e| e.to_string())?;
        }
    }

    let json = serde_json::to_vec_pretty(&settings).map_err(|e| e.to_string())?;
    crate::commands::save_local_file(&path, &json).map_err(|e| format!("保存设置失败: {e}"))
}
