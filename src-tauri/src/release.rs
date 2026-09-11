// 一键发布流水线（PRD FR-20）
//
// 把日常发版的手工五步串成一条流水线，Rust 侧单线程串行执行：
//   预检（git 工作区必须干净）
//   → checkout <发布分支>
//   → nvm use <配置版本>（记录原版本，结束后切回）
//   → bump package.json 的 version（文本替换保持原文件格式）
//   → <pm> run <构建脚本>（输出流式进日志抽屉）
//   → zip dist/ → dist/<项目名>_v<版本>_<日期>.zip
//   → nvm use <原版本>（best effort）
//
// 任一步失败即中止（已执行的步骤不回滚，弹窗会标注失败位置）；
// 每步开始/结束发 `release-step` 事件，结束后发 `release-finished`；
// 命令输出走 `log-line`（kind = "release"），与 dev 日志共用抽屉和落盘通道。

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tauri::{AppHandle, Emitter};

use crate::commands::Project;
use crate::process_manager::{compact_log_file, emit_tool_log, spawn_pipe_readers};
#[cfg(windows)]
use crate::process_manager::CREATE_NO_WINDOW;

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ReleaseStepEvent {
    project_id: String,
    step: &'static str,
    status: &'static str, // "running" | "success" | "error"
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ReleaseFinishedEvent {
    project_id: String,
    success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    zip_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

/// 流水线错误：失败步骤 + 人话原因
type PipelineError = (&'static str, String);

/// 启动发布（立即返回，流水线在后台线程跑，进度走事件）
#[tauri::command]
pub fn start_release(app: AppHandle, project: Project, new_version: String) -> Result<(), String> {
    let config = project
        .release
        .clone()
        .ok_or_else(|| "该项目未配置发布参数，请先在编辑里填写".to_string())?;
    if new_version.trim().is_empty() {
        return Err("新版本号不能为空".into());
    }
    let new_version = new_version.trim().to_string();

    thread::spawn(move || {
        // 先截断落盘日志，保证文件有界（与 start 的策略一致）
        compact_log_file(&app, &project.id);

        let result = run_pipeline(&app, &project, &config, &new_version);
        let (success, zip_path, error) = match result {
            Ok(zip) => (true, Some(zip), None),
            Err((step, msg)) => (false, None, Some(format!("{step}: {msg}"))),
        };
        let _ = app.emit(
            "release-finished",
            ReleaseFinishedEvent {
                project_id: project.id.clone(),
                success,
                zip_path,
                error,
            },
        );
    });
    Ok(())
}

fn run_pipeline(
    app: &AppHandle,
    project: &Project,
    config: &crate::commands::ReleaseConfig,
    new_version: &str,
) -> Result<String, PipelineError> {
    let dir = Path::new(&project.path);

    // 预检：工作区必须干净。不自动 stash——动用户的暂存区风险太大
    match run_capture(dir, "git status --porcelain") {
        Ok(out) if out.trim().is_empty() => {}
        Ok(_) => {
            return Err((
                "checkout",
                "工作区有未提交变更，请先提交或暂存（工具不会自动 stash）".into(),
            ))
        }
        Err(e) => return Err(("checkout", format!("无法读取 git 状态: {e}"))),
    }

    // 1. 切换发布分支
    step(app, project, "checkout", "running", None);
    run_step(app, project, dir, &format!("git checkout {}", config.branch))
        .map_err(|e| ("checkout", e))?;
    step(app, project, "checkout", "success", None);

    // 2. 切 node 版本（记录原版本用于恢复）
    step(app, project, "node", "running", None);
    let current_node = run_capture(dir, "node -v")
        .map(|v| v.trim().trim_start_matches('v').to_string())
        .map_err(|e| ("node", format!("无法获取当前 node 版本: {e}")))?;
    let need_switch = current_node != config.node_version;
    if need_switch {
        run_step(app, project, dir, &format!("nvm use {}", config.node_version))
            .map_err(|e| ("node", e))?;
    }
    step(
        app,
        project,
        "node",
        "success",
        Some(if need_switch {
            format!("{current_node} → {}", config.node_version)
        } else {
            format!("已是 {}", config.node_version)
        }),
    );

    // 3. 升版本（先改再 build，构建产物里能带上新版本号）
    step(app, project, "bump", "running", None);
    let old_version = write_package_version(&dir.join("package.json"), new_version)
        .map_err(|e| ("bump", e))?;
    step(
        app,
        project,
        "bump",
        "success",
        Some(format!("{old_version} → {new_version}")),
    );

    // 4. 构建 + 5. 压缩：任一步失败 → 还原版本号再返回错误
    //（package.json 回到发布前的状态，工作区干净，用户可直接重试）
    let outcome = (|| -> Result<String, PipelineError> {
        step(app, project, "build", "running", None);
        run_step(
            app,
            project,
            dir,
            &format!("{} run {}", project.package_manager, config.script),
        )
        .map_err(|e| ("build", e))?;
        step(app, project, "build", "success", None);

        step(app, project, "zip", "running", None);
        let dist = dir.join("dist");
        if !dist.is_dir() {
            return Err(("zip", "dist 目录不存在（构建脚本产物路径不是 dist？）".into()));
        }
        if is_dir_empty(&dist) {
            return Err(("zip", "dist 目录为空，未生成产物".into()));
        }
        let zip_path = dist.join(format!(
            "{}_v{}_{}.zip",
            sanitize_filename(&project.name),
            new_version,
            today_compact(),
        ));
        zip_dist(&dist, &zip_path).map_err(|e| ("zip", e))?;
        step(
            app,
            project,
            "zip",
            "success",
            Some(zip_path.to_string_lossy().to_string()),
        );
        Ok(zip_path.to_string_lossy().to_string())
    })();

    let outcome = match outcome {
        Ok(zip) => Ok(zip),
        Err((step_name, mut msg)) => {
            // 还原版本号（best effort：还原失败也不掩盖原始错误）
            match write_package_version(&dir.join("package.json"), &old_version) {
                Ok(restored) => {
                    emit_tool_log(
                        app,
                        &project.id,
                        "release",
                        &format!("发布失败，已还原版本号为 {restored}"),
                    );
                    msg.push_str("；版本号已还原");
                }
                Err(e) => {
                    emit_tool_log(
                        app,
                        &project.id,
                        "release",
                        &format!("还原版本号失败: {e}（当前 version 仍为 {new_version}）"),
                    );
                    msg.push_str(&format!("；版本号还原失败: {e}"));
                }
            }
            Err((step_name, msg))
        }
    };

    // 6. 恢复 node 版本（best effort：失败只提示，不影响发布结果）
    if need_switch {
        step(app, project, "restore", "running", None);
        match run_step(app, project, dir, &format!("nvm use {current_node}")) {
            Ok(()) => step(
                app,
                project,
                "restore",
                "success",
                Some(format!("已切回 {current_node}")),
            ),
            Err(e) => step(app, project, "restore", "error", Some(e)),
        }
    }

    outcome
}

fn step(
    app: &AppHandle,
    project: &Project,
    step: &'static str,
    status: &'static str,
    message: Option<String>,
) {
    let _ = app.emit(
        "release-step",
        ReleaseStepEvent {
            project_id: project.id.clone(),
            step,
            status,
            message,
        },
    );
}

/// 同步执行一步命令：stdout/stderr 转发 `log-line`（kind = "release"），
/// 等待退出，非 0 退出码返回 Err。
fn run_step(
    app: &AppHandle,
    project: &Project,
    cwd: &Path,
    command: &str,
) -> Result<(), String> {
    let mut cmd = Command::new("cmd");
    cmd.args(["/C", command])
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("执行 `{command}` 失败: {e}"))?;

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    // 擦成 Box<dyn Read> 才能放进同一个数组——两个管道类型不同
    let pipes: [(&str, Option<Box<dyn std::io::Read + Send>>); 2] =
        [("stdout", stdout.map(|p| Box::new(p) as Box<dyn std::io::Read + Send>)),
         ("stderr", stderr.map(|p| Box::new(p) as Box<dyn std::io::Read + Send>))];
    spawn_pipe_readers(app, &project.id, "release", pipes);

    match child.wait() {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => Err(format!(
            "`{command}` 退出码 {}（详见日志）",
            status.code().unwrap_or(-1)
        )),
        Err(e) => Err(format!("等待 `{command}` 退出失败: {e}")),
    }
}

/// 同步执行并捕获输出（用于 git status / node -v 这类一次性命令，输出不进日志抽屉）
fn run_capture(cwd: &Path, command: &str) -> Result<String, String> {
    let mut cmd = Command::new("cmd");
    cmd.args(["/C", command])
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let output = cmd
        .output()
        .map_err(|e| format!("执行 `{command}` 失败: {e}"))?;
    Ok(format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    ))
}

/// 把 package.json 的 version 改成 new_version。
/// 用文本替换而不是反序列化写回：保持原文件的缩进、字段顺序等格式不被重排。
fn write_package_version(pkg_path: &Path, new_version: &str) -> Result<String, String> {
    let content =
        std::fs::read_to_string(pkg_path).map_err(|e| format!("读取 package.json 失败: {e}"))?;
    let pkg: serde_json::Value = serde_json::from_str(&content)
        .map_err(|e| format!("package.json 解析失败: {e}"))?;
    let old = pkg["version"]
        .as_str()
        .ok_or("package.json 缺少 version 字段")?
        .to_string();

    for needle in [
        format!("\"version\": \"{old}\""),
        format!("\"version\":\"{old}\""),
    ] {
        if let Some(_) = content.find(&needle) {
            let replaced =
                content.replacen(&needle, &format!("\"version\": \"{new_version}\""), 1);
            std::fs::write(pkg_path, replaced)
                .map_err(|e| format!("写回 package.json 失败: {e}"))?;
            return Ok(old);
        }
    }
    Err("在 package.json 中找不到 version 字段（格式异常）".into())
}

fn is_dir_empty(dir: &Path) -> bool {
    std::fs::read_dir(dir)
        .map(|mut entries| entries.next().is_none())
        .unwrap_or(true)
}

/// 压缩产物 → zip（zip 根 = 产物内容，解压即见 index.html 等）。
/// dist 的两种形态：
/// - 版本子目录：dist/dist-<name>-<version>/（vite 按版本输出）→ 取最新子目录为根；
/// - 扁平：index.html / assets 直接在 dist 下 → 以 dist 为根。
/// 所有层级的 *.zip（历史产物，含正在生成的这份）不打入包。
fn zip_dist(dist: &Path, zip_path: &Path) -> Result<(), String> {
    use std::fs::File;
    use std::io::Write;
    use zip::write::SimpleFileOptions;
    use zip::CompressionMethod;

    let file = File::create(zip_path).map_err(|e| format!("创建 zip 失败: {e}"))?;
    let mut zip = zip::ZipWriter::new(file);
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated);

    // 版本子目录形态 → 以最新修改的子目录为 zip 根
    let root = latest_product_dir(dist).unwrap_or_else(|| dist.to_path_buf());
    walk(&root, &root, &mut zip, options)?;

    zip.finish()
        .map_err(|e| format!("完成 zip 失败: {e}"))?
        .flush()
        .map_err(|e| format!("写入 zip 失败: {e}"))?;
    Ok(())
}

/// dist 顶层只有子目录、无散文件时，返回最新修改的子目录（刚构建出来的产物）；
/// 扁平形态（顶层有散文件）返回 None。
fn latest_product_dir(dist: &Path) -> Option<PathBuf> {
    let mut dirs: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    let mut has_files = false;
    for entry in std::fs::read_dir(dist).ok()?.flatten() {
        let path = entry.path();
        if is_zip_path(&path) {
            continue;
        }
        if path.is_dir() {
            let mtime = entry.metadata().ok()?.modified().ok()?;
            dirs.push((mtime, path));
        } else {
            has_files = true;
        }
    }
    if has_files || dirs.is_empty() {
        return None;
    }
    dirs.sort_by_key(|(t, _)| *t);
    dirs.pop().map(|(_, p)| p)
}

/// 递归写入 zip。name 一律相对固定的 root 计算——基准随递归层级变化的话，
/// 不同子目录下的同名文件会算出重复的 zip 条目名（Duplicate filename 报错）。
fn walk(
    root: &Path,
    dir: &Path,
    zip: &mut zip::ZipWriter<std::fs::File>,
    options: zip::write::SimpleFileOptions,
) -> Result<(), String> {
    use std::fs::File;
    use std::io::copy;

    let entries = std::fs::read_dir(dir).map_err(|e| format!("读取目录失败: {e}"))?;
    for entry in entries.flatten() {
        let path = entry.path();
        if is_zip_path(&path) {
            continue;
        }
        let name = path
            .strip_prefix(root)
            .map_err(|e| format!("计算相对路径失败: {e}"))?
            .to_string_lossy()
            .replace('\\', "/");
        if path.is_dir() {
            zip.add_directory(format!("{name}/"), options)
                .map_err(|e| format!("写入目录项失败: {e}"))?;
            walk(root, &path, zip, options)?;
        } else {
            zip.start_file(name, options)
                .map_err(|e| format!("写入文件项失败: {e}"))?;
            let mut src = File::open(&path).map_err(|e| format!("打开文件失败: {e}"))?;
            copy(&mut src, zip).map_err(|e| format!("压缩失败: {e}"))?;
        }
    }
    Ok(())
}

fn is_zip_path(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("zip"))
}

/// 文件名非法字符替换为 '-'（项目名进 zip 文件名用）
fn sanitize_filename(name: &str) -> String {
    name.chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '-',
            _ => c,
        })
        .collect()
}

/// UTC 日期 YYYYMMDD（zip 文件名用，不引 chrono——epoch 天数转公历的 Howard Hinnant 算法）
fn today_compact() -> String {
    let days = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        / 86_400;
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { yoe + era * 400 + 1 } else { yoe + era * 400 };
    format!("{y:04}{m:02}{d:02}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn bump_replaces_version_preserving_format() {
        let path = std::env::temp_dir().join(format!(
            "fpm-pkg-test-{}.json",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::write(
            &path,
            "{\n  \"name\": \"demo\",\n  \"version\": \"1.2.3\",\n  \"scripts\": {}\n}\n",
        )
        .unwrap();
        let old = write_package_version(&path, "1.3.0").unwrap();
        assert_eq!(old, "1.2.3");
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("\"version\": \"1.3.0\""));
        assert!(content.contains("\"name\": \"demo\""));
        // 解析回 JSON 确认没写坏
        let pkg: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(pkg["version"], "1.3.0");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn sanitize_replaces_illegal_chars() {
        assert_eq!(sanitize_filename("a/b\\c:d*e?f\"g<h>i|j"), "a-b-c-d-e-f-g-h-i-j");
    }

    /// 版本子目录形态：以子目录为 zip 根，条目相对根命名，
    /// 不同子目录下的同名文件不会冲突，dist 里的 zip 不打入包
    #[test]
    fn zip_dist_versioned_subdir_layout() {
        let base = std::env::temp_dir().join(format!(
            "fpm-zip-test-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let dist = base.join("dist");
        let product = dist.join("dist-demo-1.0.0");
        // 两个不同子目录下放同名文件——旧实现的相对路径 bug 会让它们重名
        fs::create_dir_all(product.join("assets/css")).unwrap();
        fs::create_dir_all(product.join("theme/css")).unwrap();
        fs::write(product.join("assets/css/app.css"), "a").unwrap();
        fs::write(product.join("theme/css/app.css"), "b").unwrap();
        fs::write(product.join("index.html"), "<html></html>").unwrap();
        // dist 顶层的历史 zip 产物，不应被打入包
        fs::write(dist.join("old_v1.0.0_20260101.zip"), "old").unwrap();

        let zip_path = dist.join("demo_v1.0.1_20260911.zip");
        zip_dist(&dist, &zip_path).unwrap();

        let f = fs::File::open(&zip_path).unwrap();
        let archive = zip::ZipArchive::new(f).unwrap();
        let mut names: Vec<String> = archive.file_names().map(str::to_string).collect();
        names.sort();
        assert_eq!(
            names,
            vec![
                "assets/",
                "assets/css/",
                "assets/css/app.css",
                "index.html",
                "theme/",
                "theme/css/",
                "theme/css/app.css",
            ]
        );
        let _ = fs::remove_dir_all(&base);
    }

    /// 扁平形态：产物直接在 dist 下，以 dist 为根
    #[test]
    fn zip_dist_flat_layout() {
        let base = std::env::temp_dir().join(format!(
            "fpm-zip-test-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let dist = base.join("dist");
        fs::create_dir_all(dist.join("assets")).unwrap();
        fs::write(dist.join("index.html"), "<html></html>").unwrap();
        fs::write(dist.join("assets/app.js"), "console.log(1)").unwrap();

        let zip_path = dist.join("demo_v2.0.0_20260911.zip");
        zip_dist(&dist, &zip_path).unwrap();

        let f = fs::File::open(&zip_path).unwrap();
        let archive = zip::ZipArchive::new(f).unwrap();
        let mut names: Vec<String> = archive.file_names().map(str::to_string).collect();
        names.sort();
        assert_eq!(names, vec!["assets/", "assets/app.js", "index.html"]);
        let _ = fs::remove_dir_all(&base);
    }
}
