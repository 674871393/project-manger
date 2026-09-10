// 项目数据的读取与持久化（PRD FR-1）
//
// - projects.json 存放在 appDataDir；
// - inspect_project：选定文件夹后读 package.json，自动带出名称/包管理器/启动脚本/框架；
// - read_git_branch：读 .git/HEAD 解析分支名，不 spawn git 进程（PRD FR-6）。

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};
use tauri::Manager;

/// 持久化到 appDataDir/projects.json 的项目记录（与前端 Project 接口对应）
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub id: String,
    pub name: String,
    pub path: String,
    pub package_manager: String,
    pub script: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manual_port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<HashMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub framework: Option<String>,
    pub created_at: u64,
}

fn projects_file(app: &tauri::AppHandle) -> Result<std::path::PathBuf, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("无法定位应用数据目录: {e}"))?;
    Ok(dir.join("projects.json"))
}

#[tauri::command]
pub fn load_projects(app: tauri::AppHandle) -> Result<Vec<Project>, String> {
    let file = projects_file(&app)?;
    if !file.exists() {
        return Ok(Vec::new());
    }
    let content = fs::read_to_string(&file).map_err(|e| format!("读取 projects.json 失败: {e}"))?;
    serde_json::from_str(&content).map_err(|e| format!("projects.json 格式错误: {e}"))
}

#[tauri::command]
pub fn save_projects(app: tauri::AppHandle, projects: Vec<Project>) -> Result<(), String> {
    let file = projects_file(&app)?;
    if let Some(parent) = file.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("创建数据目录失败: {e}"))?;
    }
    let json = serde_json::to_string_pretty(&projects)
        .map_err(|e| format!("序列化项目列表失败: {e}"))?;
    // 先写临时文件再原子替换，避免写一半崩溃损坏数据
    let tmp = file.with_extension("json.tmp");
    fs::write(&tmp, &json).map_err(|e| format!("写入失败: {e}"))?;
    fs::rename(&tmp, &file).map_err(|e| format!("替换 projects.json 失败: {e}"))?;
    Ok(())
}

/// 选定文件夹后的自动识别结果
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectInspection {
    pub name: String,
    pub package_manager: String,
    pub script: String,
    pub framework: String,
}

#[tauri::command]
pub fn inspect_project(path: String) -> Result<ProjectInspection, String> {
    let dir = Path::new(&path);
    if !dir.is_dir() {
        return Err("文件夹不存在".into());
    }
    let pkg_path = dir.join("package.json");
    if !pkg_path.exists() {
        return Err("不是有效的前端项目：缺少 package.json".into());
    }
    let content =
        fs::read_to_string(&pkg_path).map_err(|e| format!("读取 package.json 失败: {e}"))?;
    let pkg: serde_json::Value =
        serde_json::from_str(&content).map_err(|e| format!("package.json 解析失败: {e}"))?;

    // 名称：package.json 的 name，缺省回退文件夹名
    let name = pkg["name"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| {
            dir.file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "未命名项目".into())
        });

    // 包管理器：按锁文件推断
    let package_manager = if dir.join("pnpm-lock.yaml").exists() {
        "pnpm"
    } else if dir.join("yarn.lock").exists() {
        "yarn"
    } else if dir.join("bun.lockb").exists() || dir.join("bun.lock").exists() {
        "bun"
    } else {
        "npm"
    }
    .to_string();

    // 启动脚本：优先 dev，其次 start
    let has_script = |key: &str| {
        pkg["scripts"]
            .as_object()
            .is_some_and(|s| s.contains_key(key))
    };
    let script = if has_script("dev") {
        "dev"
    } else if has_script("start") {
        "start"
    } else {
        ""
    }
    .to_string();

    // 框架：按 dependencies/devDependencies 推断，仅作展示标签
    let deps = pkg["dependencies"].as_object().cloned().unwrap_or_default();
    let dev_deps = pkg["devDependencies"]
        .as_object()
        .cloned()
        .unwrap_or_default();
    let has_dep = |key: &str| deps.contains_key(key) || dev_deps.contains_key(key);
    let framework = if has_dep("next") {
        "next"
    } else if has_dep("nuxt") {
        "nuxt"
    } else if has_dep("vite") {
        "vite"
    } else if has_dep("webpack") {
        "webpack"
    } else {
        "other"
    }
    .to_string();

    Ok(ProjectInspection {
        name,
        package_manager,
        script,
        framework,
    })
}

#[tauri::command]
pub fn read_git_branch(path: String) -> Result<Option<String>, String> {
    let head = Path::new(&path).join(".git").join("HEAD");
    if !head.exists() {
        return Ok(None);
    }
    let content = fs::read_to_string(&head).map_err(|e| format!("读取 .git/HEAD 失败: {e}"))?;
    let content = content.trim();
    if let Some(branch) = content.strip_prefix("ref: refs/heads/") {
        Ok(Some(branch.to_string()))
    } else if content.len() >= 7 && content.chars().all(|c| c.is_ascii_hexdigit()) {
        // detached HEAD：显示短哈希
        Ok(Some(content[..7].to_string()))
    } else {
        Ok(None)
    }
}

/// 目录扫描结果（复用 inspect_project 的识别逻辑）
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScannedProject {
    pub path: String,
    pub inspection: ProjectInspection,
}

/// 扫描目录下一层的子文件夹，返回所有含 package.json 的前端项目（PRD 开放问题 1 的批量导入）
#[tauri::command]
pub fn scan_projects(root: String) -> Result<Vec<ScannedProject>, String> {
    let root = Path::new(&root);
    if !root.is_dir() {
        return Err("目录不存在".into());
    }
    let mut found = Vec::new();
    let entries = fs::read_dir(root).map_err(|e| format!("读取目录失败: {e}"))?;
    for entry in entries.flatten() {
        let path = entry.path();
        // 只扫一层：如 d:\project 下的直接子文件夹
        if !path.is_dir() || !path.join("package.json").exists() {
            continue;
        }
        let path_str = path.to_string_lossy().to_string();
        if let Ok(inspection) = inspect_project(path_str.clone()) {
            found.push(ScannedProject { path: path_str, inspection });
        }
    }
    // 排序让列表稳定
    found.sort_by(|a, b| a.inspection.name.cmp(&b.inspection.name));
    Ok(found)
}
