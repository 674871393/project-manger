// 一键发布流水线（PRD FR-20）
//
// 把日常发版的手工五步串成一条流水线，Rust 侧单线程串行执行：
//   预检（git 工作区必须干净）
//   → checkout <发布分支>
//   → nvm use <配置版本>（记录原版本，结束后切回）
//   → bump package.json 的 version（文本替换保持原文件格式）
//   → <pm> run <构建脚本>（输出流式进日志抽屉）
//   → zip dist/ → dist/<项目名>_v<版本>_<日期>.zip（同日同版本重发追加 _1/_2，不覆盖）
//   → nvm use <原版本>（best effort）
//
// 版本基准是「发布分支上已提交的 package.json」，不是工作区当前检出/改了一半的那份：
// 弹窗用 read_release_context 读基准，checkout 之后 bump 前再校验一次实际版本与
// 弹窗确认的一致（expected_version），预览过期就中止而不是默默发布错误版本。
//
// 任一步失败即中止（已执行的步骤不回滚，弹窗会标注失败位置）；
// 每步开始/结束发 `release-step` 事件，结束后发 `release-finished`；
// 命令输出走 `log-line`（kind = "release"），与 dev 日志共用抽屉和落盘通道。

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;

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

/// deflate 压缩级别。实测（本仓库 dist，777 KB 前端产物）：
/// miniz_oxide level 6 = 399,156 字节 / level 9 = 397,360，NanaZip -mx9 = 392,117。
/// 6→9 只多花极少量时间就省下一点，取 9；与 7-Zip 仍差约 1%，那是 deflate
/// 编码器强度的差距（纯 Rust 的 miniz_oxide vs 7-Zip），不是级别能补的。
const ZIP_LEVEL: i64 = 9;

/// 人类可读的字节数（日志用）
fn human_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    let b = bytes as f64;
    if b >= MB {
        format!("{:.1} MB", b / MB)
    } else if b >= KB {
        format!("{:.1} KB", b / KB)
    } else {
        format!("{bytes} B")
    }
}

/// 压缩率（压缩后 / 原始），日志里与 NanaZip 的 Ratio 列对应
fn ratio(raw_bytes: u64, packed: u64) -> String {
    if raw_bytes == 0 {
        return "—".into();
    }
    format!("{:.0}%", packed as f64 / raw_bytes as f64 * 100.0)
}

/// dist 顶层是否还有子目录（有则多半是历史构建产物）
fn dist_has_subdir(dist: &Path) -> bool {
    std::fs::read_dir(dist)
        .map(|entries| {
            entries
                .flatten()
                .any(|e| e.path().is_dir())
        })
        .unwrap_or(false)
}

/// 发布弹窗的上下文：一次 invoke 取回，避免多次调用之间状态漂移
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseContext {
    /// 发布基准：发布分支上已提交的 package.json 版本
    pub release_version: String,
    /// 实际取到基准的引用（本地分支名，或回退的 origin/<分支>）
    pub release_ref: String,
    /// 当前工作区分支（detached HEAD 时为短哈希）
    pub current_branch: Option<String>,
    /// 当前工作区 package.json 的版本（可能与发布基准不同）
    pub working_version: Option<String>,
    /// 工作区是否有未提交变更（有则发布会先被预检拦下）
    pub dirty: bool,
}

/// 读发布弹窗需要的上下文（FR-20）：发布分支的版本基准 + 当前工作区状态。
/// 基准走 `git show <分支>:package.json`——工作区可能检出了别的分支、
/// 或上次发布后版本号还没提交，读工作区文件会拿到错误的基准。
#[tauri::command]
pub fn read_release_context(path: String, branch: String) -> Result<ReleaseContext, String> {
    let dir = Path::new(&path);
    if !is_safe_token(&branch, true) {
        return Err(format!(
            "发布分支「{branch}」含非法字符（仅允许字母数字与 - _ . /）"
        ));
    }

    // 本地分支优先；本地没检出该分支时回退 origin/<分支>
    // （与 `git checkout <分支>` 自动建跟踪分支的 DWIM 行为对齐）
    let mut found: Option<(String, String)> = None;
    let mut last_err = String::new();
    for reference in [branch.clone(), format!("origin/{branch}")] {
        let (ok, out, err) = run_capture(dir, &format!("git show {reference}:package.json"))
            .map_err(|e| format!("执行 git show 失败: {e}"))?;
        if ok {
            found = Some((reference, parse_version(&out)?));
            break;
        }
        last_err = err.trim().to_string();
    }
    let (release_ref, release_version) = found.ok_or_else(|| {
        format!(
            "读取发布分支版本失败（{branch} / origin/{branch}）：{last_err}；\
             请确认分支已 fetch 到本地"
        )
    })?;

    Ok(ReleaseContext {
        release_version,
        release_ref,
        current_branch: current_git_branch(dir),
        working_version: read_package_version_at(&dir.join("package.json")).ok(),
        dirty: matches!(
            run_capture(dir, "git status --porcelain"),
            Ok((true, ref out, _)) if !out.trim().is_empty()
        ),
    })
}

/// 启动发布（立即返回，流水线在后台线程跑，进度走事件）。
/// expected_version 是弹窗里确认过的版本基准，checkout 后据此校验。
#[tauri::command]
pub fn start_release(
    app: AppHandle,
    project: Project,
    new_version: String,
    expected_version: String,
) -> Result<(), String> {
    let config = project
        .release
        .clone()
        .ok_or_else(|| "该项目未配置发布参数，请先在编辑里填写".to_string())?;
    let new_version = new_version.trim().to_string();
    if new_version.is_empty() {
        return Err("新版本号不能为空".into());
    }
    let expected_version = expected_version.trim().to_string();
    if expected_version.is_empty() {
        return Err("缺少发布基准版本，请重新打开发布弹窗".into());
    }
    // 命令是 `cmd /C` 拼接执行的：分支/脚本/版本含空格或 & | 等元字符会被 cmd
    // 拆成别的命令，先做字符白名单校验（表单侧也校验，这里兜底）
    if !is_safe_token(&config.branch, true) {
        return Err(format!(
            "发布分支「{}」含非法字符（仅允许字母数字与 - _ . /）",
            config.branch
        ));
    }
    if !is_safe_token(&config.node_version, false) {
        return Err(format!(
            "node 版本「{}」含非法字符（应为形如 16.20.0 的完整版本号）",
            config.node_version
        ));
    }
    if !is_safe_token(&config.script, false) {
        return Err(format!(
            "构建脚本「{}」含非法字符（仅允许字母数字与 - _ . : @ +）",
            config.script
        ));
    }
    if !is_safe_token(&new_version, false) {
        return Err(format!(
            "新版本号「{new_version}」含非法字符（仅允许字母数字与 - _ . +）"
        ));
    }

    thread::spawn(move || {
        // 先截断落盘日志，保证文件有界（与 start 的策略一致）
        compact_log_file(&app, &project.id);

        let result = run_pipeline(&app, &project, &config, &new_version, &expected_version);
        let (success, zip_path, error) = match result {
            Ok(zip) => {
                // 成功后版本号留在工作区且未提交（PRD：不自动 commit）——
                // 不提醒的话，下次发布会被预检拦下，用户会莫名其妙
                emit_tool_log(
                    &app,
                    &project.id,
                    "release",
                    &format!(
                        "发布完成；package.json 已升为 {new_version}（未提交），\
                         请记得 commit，否则下次发布预检会中止"
                    ),
                );
                (true, Some(zip), None)
            }
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
    expected_version: &str,
) -> Result<String, PipelineError> {
    let dir = Path::new(&project.path);

    // 0. 预检：工作区必须干净。不自动 stash——动用户的暂存区风险太大
    step(app, project, "precheck", "running", None);
    let (git_ok, status_out, git_err) = run_capture(dir, "git status --porcelain")
        .map_err(|e| ("precheck", format!("无法读取 git 状态: {e}")))?;
    if !git_ok {
        // 非 0 退出码说明 git 本身报错（不是 git 仓库 / .git 损坏），诊断在 stderr
        return Err(("precheck", format!("无法读取 git 状态: {}", git_err.trim())));
    }
    if !status_out.trim().is_empty() {
        return Err((
            "precheck",
            "工作区有未提交变更，请先提交或暂存（工具不会自动 stash）".into(),
        ));
    }
    step(app, project, "precheck", "success", None);

    // 1. 切换发布分支
    step(app, project, "checkout", "running", None);
    run_step(app, project, dir, &format!("git checkout {}", config.branch))
        .map_err(|e| ("checkout", e))?;
    step(app, project, "checkout", "success", None);

    // 2. 切 node 版本（记录原版本用于恢复）
    step(app, project, "node", "running", None);
    let (node_ok, node_out, node_err) = run_capture(dir, "node -v")
        .map_err(|e| ("node", format!("无法获取当前 node 版本: {e}")))?;
    if !node_ok {
        return Err((
            "node",
            format!("无法获取当前 node 版本: {}", node_err.trim()),
        ));
    }
    let current_node = node_out.trim().trim_start_matches('v').to_string();
    let need_switch = current_node != config.node_version;
    if need_switch {
        run_step(app, project, dir, &format!("nvm use {}", config.node_version)).map_err(|e| {
            (
                "node",
                format!(
                    "{e}；请用 `nvm ls` 确认 {} 已安装。nvm-windows 切换要改 node 安装\
                     目录的符号链接，通常需要管理员权限——可以管理员身份重开本工具再试",
                    config.node_version
                ),
            )
        })?;
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
    let pkg_path = dir.join("package.json");
    // 防呆：checkout 后这份 package.json 才是发布分支的。与弹窗确认过的基准不符
    // 说明预览已过期（弹窗打开后分支被更新过）——中止，别默默发布错误版本号
    let actual = read_package_version_at(&pkg_path).map_err(|e| ("bump", e))?;
    if actual != expected_version {
        return Err((
            "bump",
            format!(
                "发布分支版本已变化：弹窗确认时是 {expected_version}，现在是 {actual}。\
                 请关闭弹窗重新打开、确认新版本号后再发布"
            ),
        ));
    }
    let old_version = write_package_version(&pkg_path, new_version).map_err(|e| ("bump", e))?;
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
        // 同日同版本重发时追加 _1/_2…，不静默覆盖历史产物
        let zip_path = unique_zip_path(
            &dist,
            format!(
                "{}_v{}_{}",
                sanitize_filename(&project.name),
                new_version,
                today_compact(),
            ),
        );
        let stats = zip_dist(&dist, &zip_path, ZIP_LEVEL).map_err(|e| ("zip", e))?;
        let packed = std::fs::metadata(&zip_path).map(|m| m.len()).unwrap_or(0);
        // 把「打进去多少东西」写进日志：产物比手压的大时，先看这行是内容多了
        // 还是压缩器弱了（跟 NanaZip 的 Original / Packed / Ratio 三列对得上）
        emit_tool_log(
            app,
            &project.id,
            "release",
            &format!(
                "压缩完成：{} 个文件，原始 {} → 压缩后 {}（{}）｜deflate 级别 {ZIP_LEVEL}｜根目录 {}",
                stats.files,
                human_size(stats.raw_bytes),
                human_size(packed),
                ratio(stats.raw_bytes, packed),
                stats.root.display(),
            ),
        );
        // 以 dist 为根且顶层还有子目录 → 历史构建产物被一并打进去了，提醒一句
        if stats.root == dist && dist_has_subdir(&dist) {
            emit_tool_log(
                app,
                &project.id,
                "release",
                "提示：dist 顶层有散文件，已按整个 dist 打包；\
                 若里面混着历史构建产物（旧版本子目录 / source map），包会明显偏大",
            );
        }
        step(
            app,
            project,
            "zip",
            "success",
            Some(format!(
                "{}（{}）",
                zip_path.to_string_lossy(),
                human_size(packed)
            )),
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

/// 同步执行并捕获输出（预检 / 读 node 版本这类一次性命令，输出不进日志抽屉）。
/// 返回 (退出码是否为 0, stdout, stderr)——调用方必须先看退出码：
/// 失败命令的诊断在 stderr，把两个流混在一起判断会把「git 报错」误当成「有变更」。
fn run_capture(cwd: &Path, command: &str) -> Result<(bool, String, String), String> {
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
    Ok((
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    ))
}

/// 从 package.json 文本里取 version
fn parse_version(content: &str) -> Result<String, String> {
    let pkg: serde_json::Value =
        serde_json::from_str(content).map_err(|e| format!("package.json 解析失败: {e}"))?;
    pkg["version"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| "package.json 缺少 version 字段".into())
}

/// 读某个 package.json 的 version（不改文件）
fn read_package_version_at(pkg_path: &Path) -> Result<String, String> {
    let content =
        std::fs::read_to_string(pkg_path).map_err(|e| format!("读取 package.json 失败: {e}"))?;
    parse_version(&content)
}

/// 当前分支。用 `git rev-parse` 而不是读 `.git/HEAD`：worktree 下 `.git` 是文件，
/// 顺着路径找 HEAD 找不到；detached HEAD 时 `--abbrev-ref` 返回字面量 "HEAD"，
/// 退化为短哈希。
fn current_git_branch(dir: &Path) -> Option<String> {
    if let Ok((true, out, _)) = run_capture(dir, "git rev-parse --abbrev-ref HEAD") {
        let name = out.trim();
        if !name.is_empty() && name != "HEAD" {
            return Some(name.to_string());
        }
    }
    match run_capture(dir, "git rev-parse --short HEAD") {
        Ok((true, out, _)) if !out.trim().is_empty() => Some(out.trim().to_string()),
        _ => None,
    }
}

/// 把 package.json 的 version 改成 new_version。
/// 用文本替换而不是反序列化写回：保持原文件的缩进、字段顺序等格式不被重排。
/// 定位时容忍 `"version" : "1.2.3"` 这类冒号两侧的空白，且只替换值本身，
/// 键周围的原始格式原样保留。
fn write_package_version(pkg_path: &Path, new_version: &str) -> Result<String, String> {
    let content =
        std::fs::read_to_string(pkg_path).map_err(|e| format!("读取 package.json 失败: {e}"))?;
    let old = parse_version(&content)?;

    let key = "\"version\"";
    let quoted_old = format!("\"{old}\"");
    let bytes = content.as_bytes();
    let mut from = 0;
    while let Some(offset) = content[from..].find(key) {
        let key_at = from + offset;
        let mut i = key_at + key.len();
        // 键与冒号之间、冒号与值之间的空白都跳过
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i < bytes.len() && bytes[i] == b':' {
            i += 1;
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            if content[i..].starts_with(&quoted_old) {
                let replaced = format!(
                    "{}\"{new_version}\"{}",
                    &content[..i],
                    &content[i + quoted_old.len()..],
                );
                std::fs::write(pkg_path, replaced)
                    .map_err(|e| format!("写回 package.json 失败: {e}"))?;
                return Ok(old);
            }
        }
        // 这里不是 version 键值对（如字符串值里恰好含 "version"），继续往后找
        from = key_at + key.len();
    }
    Err("在 package.json 中找不到 version 字段（格式异常）".into())
}

fn is_dir_empty(dir: &Path) -> bool {
    std::fs::read_dir(dir)
        .map(|mut entries| entries.next().is_none())
        .unwrap_or(true)
}

/// 一次压缩的统计：写进日志，回答「为什么这个 zip 比手压的大」
struct ZipStats {
    /// 实际当作 zip 根的目录
    root: PathBuf,
    /// 打进包的文件数（不含目录项）
    files: u64,
    /// 打进包的文件原始总字节
    raw_bytes: u64,
}

/// 压缩产物 → zip（zip 根 = 产物内容，解压即见 index.html 等）。
/// dist 的两种形态：
/// - 版本子目录：dist/dist-<name>-<version>/（vite 按版本输出）→ 取最新子目录为根；
/// - 扁平：index.html / assets 直接在 dist 下 → 以 dist 为根。
/// 所有层级的 *.zip（历史产物，含正在生成的这份）不打入包。
fn zip_dist(dist: &Path, zip_path: &Path, level: i64) -> Result<ZipStats, String> {
    use std::fs::File;
    use std::io::Write;
    use zip::write::SimpleFileOptions;
    use zip::CompressionMethod;

    let file = File::create(zip_path).map_err(|e| format!("创建 zip 失败: {e}"))?;
    let mut zip = zip::ZipWriter::new(file);
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .compression_level(Some(level));

    // 版本子目录形态 → 以最新修改的子目录为 zip 根
    let root = latest_product_dir(dist).unwrap_or_else(|| dist.to_path_buf());
    let mut stats = ZipStats {
        root: root.clone(),
        files: 0,
        raw_bytes: 0,
    };
    walk(&root, &root, &mut zip, options, &mut stats)?;

    zip.finish()
        .map_err(|e| format!("完成 zip 失败: {e}"))?
        .flush()
        .map_err(|e| format!("写入 zip 失败: {e}"))?;
    Ok(stats)
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
    stats: &mut ZipStats,
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
            walk(root, &path, zip, options, stats)?;
        } else {
            stats.files += 1;
            stats.raw_bytes += std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
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

/// 生成不与现有文件冲突的 zip 路径：同日同版本重发时追加 _1/_2…
fn unique_zip_path(dist: &Path, stem: String) -> PathBuf {
    let first = dist.join(format!("{stem}.zip"));
    if !first.exists() {
        return first;
    }
    (1..)
        .map(|n| dist.join(format!("{stem}_{n}.zip")))
        .find(|p| !p.exists())
        .expect("序号递增不可能用尽")
}

/// `cmd /C` 拼接执行前的白名单校验：空串，或含空格 / & | < > ^ % " 等字符
/// 会被 cmd 拆成别的命令，一律拒绝。分支名额外允许 `/`（feature/x）。
fn is_safe_token(s: &str, allow_slash: bool) -> bool {
    !s.is_empty()
        && s.chars().all(|c| {
            c.is_ascii_alphanumeric()
                || matches!(c, '-' | '_' | '.' | ':' | '@' | '+')
                || (allow_slash && c == '/')
        })
}

/// 本地日期 YYYYMMDD（zip 文件名用；chrono 本就是依赖树里的传递依赖，
/// 之前手算 epoch 转公历还用的是 UTC，东八区早八点前发布的日期会差一天）
fn today_compact() -> String {
    chrono::Local::now().format("%Y%m%d").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

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

    /// 冒号两侧带空白的 format 也能定位并替换，且只动值、保留原格式
    #[test]
    fn bump_tolerates_space_around_colon() {
        let path = std::env::temp_dir().join(format!(
            "fpm-pkg-space-test-{}.json",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::write(
            &path,
            "{\n  \"name\": \"demo\",\n  \"version\" : \"1.2.3\",\n  \"scripts\": {}\n}\n",
        )
        .unwrap();
        let old = write_package_version(&path, "1.3.0").unwrap();
        assert_eq!(old, "1.2.3");
        let content = fs::read_to_string(&path).unwrap();
        // 冒号前的空格原样保留
        assert!(content.contains("\"version\" : \"1.3.0\""));
        let pkg: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(pkg["version"], "1.3.0");
        let _ = fs::remove_file(&path);
    }

    /// 字符串值里恰好含 "version" 但后面不是 `"旧版本"` 时跳过，不误替换
    #[test]
    fn bump_skips_fake_version_keys() {
        let path = std::env::temp_dir().join(format!(
            "fpm-pkg-fake-test-{}.json",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::write(
            &path,
            "{\n  \"note\": \"the version field\",\n  \"version\": \"2.0.0\"\n}\n",
        )
        .unwrap();
        let old = write_package_version(&path, "2.1.0").unwrap();
        assert_eq!(old, "2.0.0");
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("the version field"));
        assert!(content.contains("\"version\": \"2.1.0\""));
        let _ = fs::remove_file(&path);
    }

    /// 同名 zip 已存在时追加 _1，不覆盖
    #[test]
    fn unique_zip_path_appends_suffix() {
        let base = std::env::temp_dir().join(format!(
            "fpm-zip-unique-test-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&base).unwrap();
        fs::write(base.join("demo_v1.0.0_20260914.zip"), "old").unwrap();

        let first = unique_zip_path(&base, "demo_v1.0.0_20260914".to_string());
        assert_eq!(
            first.file_name().unwrap().to_string_lossy(),
            "demo_v1.0.0_20260914_1.zip"
        );

        // 无冲突时直接用原名
        fs::remove_file(base.join("demo_v1.0.0_20260914.zip")).unwrap();
        let second = unique_zip_path(&base, "demo_v1.0.0_20260914".to_string());
        assert_eq!(
            second.file_name().unwrap().to_string_lossy(),
            "demo_v1.0.0_20260914.zip"
        );
        let _ = fs::remove_dir_all(&base);
    }

    /// 缺少 version 字段时报错而不是 panic（发布基准读不出来要中止）
    #[test]
    fn parse_version_rejects_missing_field() {
        assert_eq!(parse_version("{\"version\": \"1.0.0\"}").unwrap(), "1.0.0");
        assert!(parse_version("{\"name\": \"demo\"}").is_err());
        assert!(parse_version("not json").is_err());
    }

    /// 版本基准必须来自发布分支的已提交内容，而不是工作区当前检出的那份。
    /// 建真仓库跑：master 是 1.4.0，切到 feature 后改到 2.1.0，读 master 应得 1.4.0。
    #[test]
    fn release_context_reads_base_from_release_branch() {
        // 需要 git；环境里没有就跳过（这条流水线本身也依赖 git）
        if !matches!(run_capture(Path::new("."), "git --version"), Ok((true, _, _))) {
            return;
        }
        let base = std::env::temp_dir().join(format!(
            "fpm-ctx-test-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let repo = base.join("repo");
        fs::create_dir_all(&repo).unwrap();
        // 临时仓库自带身份，不依赖全局 git config
        let git = |cmd: &str| {
            let full = format!("git -c user.email=t@t -c user.name=t {cmd}");
            let (ok, _, err) = run_capture(&repo, &full).unwrap();
            assert!(ok, "`{full}` 失败: {}", err.trim());
        };

        git("init -q");
        fs::write(repo.join("package.json"), "{\"version\": \"1.4.0\"}").unwrap();
        git("add -A");
        git("commit -q -m init");
        git("branch -M master");
        git("checkout -q -b feature");
        fs::write(repo.join("package.json"), "{\"version\": \"2.1.0\"}").unwrap();
        git("add -A");
        git("commit -q -m bump-on-feature");

        let path = repo.to_string_lossy().to_string();
        let ctx = read_release_context(path.clone(), "master".to_string()).unwrap();
        assert_eq!(ctx.release_version, "1.4.0", "基准应取发布分支的已提交版本");
        assert_eq!(ctx.release_ref, "master");
        assert_eq!(ctx.current_branch.as_deref(), Some("feature"));
        assert_eq!(ctx.working_version.as_deref(), Some("2.1.0"));
        assert!(!ctx.dirty);

        // 工作区脏（上次发布后版本号没提交）也能读出来，供弹窗提示
        fs::write(repo.join("package.json"), "{\"version\": \"2.1.1\"}").unwrap();
        let ctx = read_release_context(path, "master".to_string()).unwrap();
        assert_eq!(ctx.release_version, "1.4.0", "脏工作区不影响基准");
        assert_eq!(ctx.working_version.as_deref(), Some("2.1.1"));
        assert!(ctx.dirty);

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn safe_token_rejects_cmd_metacharacters() {
        assert!(is_safe_token("feature/release-1.2", true));
        assert!(is_safe_token("build:prod", false));
        assert!(is_safe_token("16.20.0", false));
        // 空格 / 元字符 / 空串都会被 cmd 拆出别的命令
        assert!(!is_safe_token("main && del *", true));
        assert!(!is_safe_token("a|b", true));
        assert!(!is_safe_token("", false));
        // `/` 只对分支名放行
        assert!(!is_safe_token("a/b", false));
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
        zip_dist(&dist, &zip_path, ZIP_LEVEL).unwrap();

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
        zip_dist(&dist, &zip_path, ZIP_LEVEL).unwrap();

        let f = fs::File::open(&zip_path).unwrap();
        let archive = zip::ZipArchive::new(f).unwrap();
        let mut names: Vec<String> = archive.file_names().map(str::to_string).collect();
        names.sort();
        assert_eq!(names, vec!["assets/", "assets/app.js", "index.html"]);
        let _ = fs::remove_dir_all(&base);
    }
}
