// 本地发布：校验 Git 工作区，在构建子进程中选定 Node，打包本轮产物并记录结果。

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::thread;
use std::time::Instant;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};

use crate::commands::{Project, ReleaseConfig};
use crate::diagnostics::{git_head, hide_console};
#[cfg(windows)]
use crate::process_manager::CREATE_NO_WINDOW;
use crate::process_manager::{
    compact_log_file, emit_tool_log, spawn_pipe_readers, validate_package_manager,
};
use crate::release_announcements::{
    normalize_version_log_path, version_log_at_ref, ReleaseAnnouncement, VersionLogPreview,
};
use crate::release_artifacts::{select_artifacts, snapshot, validate_output_dir, write_archive};
use crate::release_files::{
    package_version, read_regular_file, update_package_version, with_release_file_edits,
    PipelineError, ReleaseFileEdit,
};
use crate::release_history::{create_history_log, save_record, ReleaseRecord};
use crate::release_node::{build_command, resolve_node, validate_node_version};

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

/// 发布弹窗的上下文：一次 invoke 取回，避免多次调用之间状态漂移
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReleaseContext {
    /// 发布基准：发布分支上已提交的 package.json 版本
    pub release_version: String,
    /// 实际取到基准的引用（本地分支名，或回退的 origin/<分支>）
    pub release_ref: String,
    /// 当前工作区分支（detached HEAD 时为短哈希）
    pub current_branch: Option<String>,
    /// 当前工作区 package.json 的版本（可能与发布基准不同）
    pub working_version: Option<String>,
    /// 是否还有会拦住发布的未提交变更（已忽略的文件不算）
    pub dirty: bool,
    /// 会拦住发布的路径
    pub blocking: Vec<String>,
    /// 预检已忽略的路径
    pub ignored: Vec<String>,
    /// pnpm-workspace.yaml 的差异只有 allowBuilds 块
    pub allow_builds_ignored: bool,
    pub allow_builds_changes: Vec<AllowBuildsChange>,
    pub release_date: String,
    pub version_log: Option<VersionLogPreview>,
}

/// 读发布弹窗需要的上下文（FR-20）：发布分支的版本基准 + 当前工作区状态。
/// 基准走 `git show <分支>:package.json`——工作区可能检出了别的分支、
/// 或上次发布后版本号还没提交，读工作区文件会拿到错误的基准。
#[tauri::command]
pub(crate) fn read_release_context(
    path: String,
    branch: String,
    ignore_paths: Option<Vec<String>>,
    version_log_path: Option<String>,
) -> Result<ReleaseContext, String> {
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
        let (ok, out, err) = run_capture(dir, &format!("git show {reference}:./package.json"))
            .map_err(|e| format!("执行 git show 失败: {e}"))?;
        if ok {
            found = Some((reference, package_version(&out)?));
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

    let ws = workspace_status(dir, ignore_paths.as_deref().unwrap_or(&[]))?;
    let version_log = version_log_at_ref(dir, &release_ref, version_log_path.as_deref())?
        .map(|source| source.preview())
        .transpose()?;

    Ok(ReleaseContext {
        release_version,
        release_ref,
        current_branch: git_head(dir).ok().flatten(),
        working_version: read_regular_file(&dir.join("package.json"))
            .and_then(|content| package_version(&content))
            .ok(),
        dirty: !ws.blocking.is_empty(),
        blocking: ws.blocking,
        ignored: ws.ignored,
        allow_builds_ignored: ws.allow_builds_ignored,
        allow_builds_changes: ws.allow_builds_changes,
        release_date: chrono::Local::now().format("%Y-%m-%d").to_string(),
        version_log,
    })
}

#[derive(Default)]
pub(crate) struct ReleaseManager(Mutex<Option<String>>);

impl ReleaseManager {
    pub(crate) fn active_project(&self) -> Option<String> {
        self.0.lock().unwrap().clone()
    }

    fn reserve(&self, project_id: &str) -> Result<(), String> {
        let mut active = self.0.lock().unwrap();
        if active.is_some() {
            return Err("已有发布任务正在运行，请等它结束后再开始".into());
        }
        *active = Some(project_id.into());
        Ok(())
    }

    fn finish(&self) {
        *self.0.lock().unwrap() = None;
    }
}

#[tauri::command]
pub(crate) fn get_active_release(state: State<ReleaseManager>) -> Option<String> {
    state.active_project()
}

#[tauri::command]
pub(crate) fn start_release(
    app: AppHandle,
    project: Project,
    new_version: String,
    expected_version: String,
    announcement: Option<ReleaseAnnouncement>,
) -> Result<(), String> {
    let config = project.release.clone().ok_or("该项目未配置发布参数")?;
    let new_version = new_version.trim().to_owned();
    let expected_version = expected_version.trim().to_owned();
    validate_release(&project, &config, &new_version, &expected_version)?;
    app.state::<ReleaseManager>().reserve(&project.id)?;

    let worker_app = app.clone();
    let spawned = thread::Builder::new()
        .name("local-release".into())
        .spawn(move || {
            execute_release(
                worker_app,
                project,
                config,
                new_version,
                expected_version,
                announcement,
            );
        });
    if let Err(error) = spawned {
        app.state::<ReleaseManager>().finish();
        return Err(format!("启动发布线程失败: {error}"));
    }
    Ok(())
}

fn validate_release(
    project: &Project,
    config: &ReleaseConfig,
    version: &str,
    expected: &str,
) -> Result<(), String> {
    validate_package_manager(&project.package_manager)?;
    if !is_safe_token(&config.branch, true) {
        return Err("发布分支格式无效".into());
    }
    if !validate_node_version(&config.node_version) {
        return Err("请填写完整 Node 版本号，例如 20.18.0".into());
    }
    if !is_safe_token(&config.script, false) {
        return Err("构建脚本名格式无效".into());
    }
    if !valid_release_version(version) {
        return Err("新版本号格式无效".into());
    }
    if expected.is_empty() {
        return Err("缺少发布基准版本，请重新检查".into());
    }
    validate_output_dir(&config.output_dir)?;
    if let Some(path) = config
        .version_log_path
        .as_deref()
        .filter(|path| !path.is_empty())
    {
        normalize_version_log_path(path)?;
    }
    Ok(())
}

fn valid_release_version(version: &str) -> bool {
    version
        .bytes()
        .next()
        .is_some_and(|byte| byte.is_ascii_digit())
        && version
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b".+-".contains(&byte))
        && !version.ends_with(['.', '+', '-'])
}

fn execute_release(
    app: AppHandle,
    project: Project,
    config: ReleaseConfig,
    version: String,
    expected: String,
    announcement: Option<ReleaseAnnouncement>,
) {
    compact_log_file(&app, &project.id);
    let started = Instant::now();
    let mut record = ReleaseRecord::new(&project, &config, &version);
    let history_log = create_history_log(&app, &record.id);
    let mut task = ReleaseTask {
        app: &app,
        project: &project,
        history_log: history_log.as_deref().ok(),
        commit: None,
    };
    let result = match history_log.as_ref() {
        Ok(_) => run_pipeline(
            &mut task,
            &config,
            &version,
            &expected,
            announcement.as_ref(),
        ),
        Err(error) => Err(("precheck", error.clone())),
    };
    match result {
        Ok(zip) => {
            let files = if announcement.is_some() {
                "版本号和更新公告"
            } else {
                "package.json 版本号"
            };
            task.log(&format!("发布完成，{files}已同步为 {version}，请按需提交"));
            record.success = true;
            record.zip_path = Some(zip);
        }
        Err((step, error)) => {
            task.step(step, "error", Some(error.clone()));
            task.log(&error);
            record.error = Some(format!("{step}: {error}"));
        }
    }
    record.commit = task.commit.take();
    record.duration_ms = started.elapsed().as_millis() as u64;
    if let Err(error) = save_record(&app, &record) {
        task.log(&error);
    }
    app.state::<ReleaseManager>().finish();
    let _ = app.emit(
        "release-finished",
        ReleaseFinishedEvent {
            project_id: record.project_id,
            success: record.success,
            zip_path: record.zip_path,
            error: record.error,
        },
    );
}

struct ReleaseTask<'a> {
    app: &'a AppHandle,
    project: &'a Project,
    history_log: Option<&'a Path>,
    commit: Option<String>,
}

impl ReleaseTask<'_> {
    fn log(&self, line: &str) {
        emit_tool_log(self.app, &self.project.id, "release", line);
        if let Some(path) = self.history_log {
            if let Ok(mut file) = std::fs::OpenOptions::new().append(true).open(path) {
                let _ = writeln!(file, "[release] {line}");
            }
        }
    }

    fn step(&self, step: &'static str, status: &'static str, message: Option<String>) {
        let _ = self.app.emit(
            "release-step",
            ReleaseStepEvent {
                project_id: self.project.id.clone(),
                step,
                status,
                message,
            },
        );
    }

    fn run(&self, command: &mut Command, label: &str) -> Result<(), String> {
        self.log(&format!("执行 {label}"));
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        hide_console(command);
        let mut child = command
            .spawn()
            .map_err(|e| format!("执行 {label} 失败: {e}"))?;
        let pipes: [(&str, Option<Box<dyn std::io::Read + Send>>); 2] = [
            (
                "stdout",
                child.stdout.take().map(|pipe| Box::new(pipe) as _),
            ),
            (
                "stderr",
                child.stderr.take().map(|pipe| Box::new(pipe) as _),
            ),
        ];
        let readers = spawn_pipe_readers(
            self.app,
            &self.project.id,
            "release",
            None,
            self.history_log,
            pipes,
        );
        let status = child
            .wait()
            .map_err(|e| format!("等待 {label} 结束失败: {e}"))?;
        for reader in readers {
            let _ = reader.join();
        }
        if status.success() {
            Ok(())
        } else {
            Err(format!(
                "{label} 退出码 {}（详见日志）",
                status.code().unwrap_or(-1)
            ))
        }
    }
}

fn run_pipeline(
    task: &mut ReleaseTask<'_>,
    config: &ReleaseConfig,
    version: &str,
    expected: &str,
    announcement: Option<&ReleaseAnnouncement>,
) -> Result<String, PipelineError> {
    let dir = Path::new(&task.project.path);
    task.step("precheck", "running", None);
    let workspace = workspace_status(dir, &config.ignore_paths).map_err(|e| ("precheck", e))?;
    if !workspace.blocking.is_empty() {
        return Err((
            "precheck",
            format!(
                "工作区有未提交变更（{}），请先提交或 stash 后再发布",
                join_paths(&workspace.blocking, 12)
            ),
        ));
    }
    task.step("precheck", "success", None);

    task.step("node", "running", None);
    let node = resolve_node(&config.node_version).map_err(|e| ("node", e))?;
    let mut build = build_command(task.project, &node, &config.script).map_err(|e| ("node", e))?;
    task.step(
        "node",
        "success",
        Some(format!("本次构建使用 Node {}", config.node_version)),
    );

    task.step("checkout", "running", None);
    let mut checkout = Command::new("git");
    checkout.args(["checkout", &config.branch]).current_dir(dir);
    task.run(&mut checkout, &format!("git checkout {}", config.branch)).map_err(|error| {
        if workspace.allow_builds_ignored {
            ("checkout", format!("{error}；如发布分支上该配置不同，请先审查并提交 pnpm-workspace.yaml 的改动"))
        } else { ("checkout", error) }
    })?;
    task.commit = match run_capture(dir, "git rev-parse HEAD") {
        Ok((true, out, _)) => Some(out.trim().into()),
        _ => None,
    };
    task.step(
        "checkout",
        "success",
        task.commit
            .as_ref()
            .map(|commit| commit.chars().take(8).collect()),
    );

    task.step("bump", "running", None);
    let edits = prepare_release_edits(
        dir,
        config.version_log_path.as_deref(),
        version,
        expected,
        announcement,
    )
    .map_err(|error| ("bump", error))?;
    with_release_file_edits(&edits, || {
        let details = if announcement.is_some() {
            " · 已同步公告"
        } else {
            ""
        };
        task.step(
            "bump",
            "success",
            Some(format!("{expected} → {version}{details}")),
        );
        build_and_package(task, config, version, &mut build, dir)
    })
}

fn prepare_release_edits(
    dir: &Path,
    configured_log: Option<&str>,
    version: &str,
    expected: &str,
    announcement: Option<&ReleaseAnnouncement>,
) -> Result<Vec<ReleaseFileEdit>, String> {
    let package = dir.join("package.json");
    let original = read_regular_file(&package)?;
    let actual = package_version(&original)?;
    if actual != expected {
        return Err(format!(
            "发布版本基准已变化，确认时为 {expected}，当前为 {actual}。请重新检查"
        ));
    }
    let updated = update_package_version(&original, version)?;
    let mut edits = vec![ReleaseFileEdit::new(package, original, updated)];
    match (
        version_log_at_ref(dir, "HEAD", configured_log)?,
        announcement,
    ) {
        (Some(source), Some(draft)) => edits.push(source.prepare_edit(dir, version, draft)?),
        (Some(_), None) => return Err("请先确认本次更新公告，再开始发布".into()),
        (None, Some(_)) => return Err("发布分支的版本日志配置已变化，请重新检查".into()),
        (None, None) => {}
    }
    Ok(edits)
}

fn build_and_package(
    task: &ReleaseTask<'_>,
    config: &ReleaseConfig,
    version: &str,
    build: &mut Command,
    dir: &Path,
) -> Result<String, PipelineError> {
    task.step("build", "running", None);
    let output_path = validate_output_dir(&config.output_dir).map_err(|e| ("build", e))?;
    let before = snapshot(dir, output_path).map_err(|e| ("build", e))?;
    task.run(
        build,
        &format!("{} run {}", task.project.package_manager, config.script),
    )
    .map_err(|e| ("build", e))?;
    task.step("build", "success", None);

    task.step("zip", "running", None);
    let output = dir.join(output_path);
    let after = snapshot(dir, output_path).map_err(|e| ("zip", e))?;
    let artifacts = select_artifacts(&output, &before, after, version).map_err(|e| ("zip", e))?;
    let stem = format!(
        "{}_v{}_{}",
        sanitize_filename(&task.project.name),
        version,
        today_compact()
    );
    let zip_path = unique_zip_path(&output, stem);
    let stats = write_archive(&artifacts, &zip_path, ZIP_LEVEL).map_err(|e| ("zip", e))?;
    task.log(&format!(
        "压缩完成：{} 个文件，{} → {}（{}），产物根目录 {}",
        stats.files,
        human_size(stats.raw_bytes),
        human_size(stats.packed_bytes),
        ratio(stats.raw_bytes, stats.packed_bytes),
        artifacts.root.display(),
    ));
    task.step(
        "zip",
        "success",
        Some(format!(
            "{}（{}）",
            zip_path.display(),
            human_size(stats.packed_bytes)
        )),
    );
    Ok(zip_path.to_string_lossy().into_owned())
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

/// `git status --porcelain` 里会拦住发布的路径、已忽略的路径，以及
/// pnpm-workspace.yaml 是不是「只有 allowBuilds 变了」。
struct WorkspaceStatus {
    blocking: Vec<String>,
    ignored: Vec<String>,
    allow_builds_ignored: bool,
    allow_builds_changes: Vec<AllowBuildsChange>,
}

fn workspace_status(dir: &Path, extra_ignore: &[String]) -> Result<WorkspaceStatus, String> {
    // -uall：否则未跟踪目录只显示成 `?? apps/`，里面的 .env.development 对不上忽略规则，
    // 整个目录会被误当成待提交代码。已跟踪文件的改动不受这个开关影响。
    let (ok, out, err) = run_capture(dir, "git status --porcelain --untracked-files=all")
        .map_err(|e| format!("无法读取 git 状态: {e}"))?;
    if !ok {
        return Err(format!("无法读取 git 状态: {}", err.trim()));
    }

    let mut blocking = Vec::new();
    let mut ignored = Vec::new();
    let mut saw_workspace = false;
    for path in parse_porcelain_paths(&out) {
        if path_ignored(&path, extra_ignore) {
            ignored.push(path);
            continue;
        }
        if path == "pnpm-workspace.yaml" {
            saw_workspace = true;
            continue;
        }
        blocking.push(path);
    }

    let (allow_builds_ignored, allow_builds_changes) = if saw_workspace {
        classify_workspace_file(dir)?
    } else {
        (false, Vec::new())
    };
    if saw_workspace && allow_builds_ignored {
        ignored.push("pnpm-workspace.yaml".into());
    } else if saw_workspace {
        blocking.push("pnpm-workspace.yaml".into());
    }
    blocking.sort();
    ignored.sort();
    Ok(WorkspaceStatus {
        blocking,
        ignored,
        allow_builds_ignored,
        allow_builds_changes,
    })
}

fn join_paths(paths: &[String], limit: usize) -> String {
    let shown: Vec<&str> = paths.iter().take(limit).map(String::as_str).collect();
    let mut text = shown.join("、");
    if paths.len() > limit {
        text.push_str(&format!(" 等 {} 个", paths.len()));
    }
    text
}

/// porcelain v1：`XY PATH`，重命名取箭头右侧。路径含特殊字符时 git 会加引号。
fn parse_porcelain_paths(out: &str) -> Vec<String> {
    let mut paths = Vec::new();
    for line in out.lines() {
        if line.len() < 4 {
            continue;
        }
        let status = &line[..2];
        let payload = line[3..].trim_end();
        let path = if status.contains('R') || status.contains('C') {
            payload
                .rsplit_once(" -> ")
                .map(|(_, new)| new)
                .unwrap_or(payload)
        } else {
            payload
        };
        let path = unquote_git(path);
        if !path.is_empty() {
            paths.push(path);
        }
    }
    paths
}

fn unquote_git(s: &str) -> String {
    let s = s.trim();
    if s.len() < 2 || !s.starts_with('"') || !s.ends_with('"') {
        return s.to_string();
    }
    let mut out = String::new();
    let mut chars = s[1..s.len() - 1].chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('"') => out.push('"'),
                Some('\\') => out.push('\\'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// 内置始终忽略 `.env.development`（任意目录）。额外模式：
/// 无斜杠 = 文件名或整段路径；带 `*` 的简单 glob（`*` 不跨 `/`，`**` 跨目录）；
/// 以 `/` 结尾 = 目录前缀；否则整段路径精确匹配。
fn path_ignored(path: &str, extra: &[String]) -> bool {
    let path = path.trim_end_matches(['/', '\\']).replace('\\', "/");
    let name = path.rsplit('/').next().unwrap_or(&path);
    if name == ".env.development" {
        return true;
    }
    extra.iter().any(|raw| {
        let pattern = raw.trim().replace('\\', "/");
        if pattern.is_empty() || pattern_too_broad(&pattern) {
            return false;
        }
        if pattern.contains('*') {
            glob_match(&pattern, &path) || (!pattern.contains('/') && glob_match(&pattern, name))
        } else if pattern.ends_with('/') {
            let prefix = pattern.trim_end_matches('/');
            path == prefix || path.starts_with(&format!("{prefix}/"))
        } else if pattern.contains('/') {
            path == pattern
        } else {
            name == pattern || path == pattern
        }
    })
}

/// `*` / `**` 会把所有未提交文件都放行，等于关掉预检，拒绝这种模式。
fn pattern_too_broad(pattern: &str) -> bool {
    matches!(pattern, "*" | "**" | "**/*" | "**/**" | "/*" | "/**")
}

fn glob_match(pattern: &str, text: &str) -> bool {
    fn rec(p: &[char], t: &[char]) -> bool {
        let mut i = 0;
        let mut j = 0;
        while i < p.len() {
            if p[i] == '*' && i + 1 < p.len() && p[i + 1] == '*' {
                let mut k = i + 2;
                if k < p.len() && p[k] == '/' {
                    k += 1;
                }
                if k == p.len() {
                    return true;
                }
                for t0 in j..=t.len() {
                    if rec(&p[k..], &t[t0..]) {
                        return true;
                    }
                }
                return false;
            }
            if p[i] == '*' {
                let rest = &p[i + 1..];
                let end = t[j..]
                    .iter()
                    .position(|c| *c == '/')
                    .map(|n| j + n)
                    .unwrap_or(t.len());
                for t0 in j..=end {
                    if rec(rest, &t[t0..]) {
                        return true;
                    }
                }
                return false;
            }
            if j >= t.len() || p[i] != t[j] {
                return false;
            }
            i += 1;
            j += 1;
        }
        j == t.len()
    }
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    rec(&p, &t)
}

struct AllowSplit {
    /// allowBuilds 块以外的文本（换行已规范成 `\n`）
    rest: String,
    entries: BTreeMap<String, String>,
}

/// 把根级 `allowBuilds:` 块从正文里拆出来。块外文本相同 ⇒ 文件没有别的改动。
fn split_allow_builds(content: &str) -> AllowSplit {
    let normalized = content.replace("\r\n", "\n");
    let mut rest_lines: Vec<&str> = Vec::new();
    let mut entries = BTreeMap::new();
    let mut in_block = false;
    for line in normalized.split('\n') {
        if !in_block {
            if is_allow_builds_header(line) {
                in_block = true;
            } else {
                rest_lines.push(line);
            }
            continue;
        }
        if is_root_line(line) {
            in_block = false;
            if is_allow_builds_header(line) {
                continue;
            }
            rest_lines.push(line);
            continue;
        }
        match parse_allow_entry(line) {
            AllowEntry::Skip => {}
            AllowEntry::Value(k, v) => {
                entries.insert(k, v);
            }
            AllowEntry::Other => rest_lines.push(line),
        }
    }
    AllowSplit {
        rest: rest_lines.join("\n"),
        entries,
    }
}

fn is_root_line(line: &str) -> bool {
    let trimmed = line.trim_end();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return false;
    }
    !trimmed.starts_with(' ') && !trimmed.starts_with('\t')
}

fn is_allow_builds_header(line: &str) -> bool {
    root_key_name(line).as_deref() == Some("allowBuilds")
        && line.split_once(':').is_some_and(|(_, value)| {
            value.trim().is_empty() || value.trim_start().starts_with('#')
        })
}

fn root_key_name(line: &str) -> Option<String> {
    if !is_root_line(line) {
        return None;
    }
    let t = line.trim();
    let (name, rest) = if let Some(r) = t.strip_prefix('"').or_else(|| t.strip_prefix('\'')) {
        let quote = t.chars().next()?;
        let end = r.find(quote)?;
        (r[..end].to_string(), r[end + 1..].trim_start().to_string())
    } else {
        let idx = t.find(':')?;
        (
            t[..idx].trim().to_string(),
            t[idx..].trim_start().to_string(),
        )
    };
    if rest.starts_with(':') {
        Some(name)
    } else {
        None
    }
}

enum AllowEntry {
    Skip,
    Value(String, String),
    Other,
}

fn parse_allow_entry(line: &str) -> AllowEntry {
    let t = line.trim();
    if t.is_empty() || t.starts_with('#') {
        return AllowEntry::Skip;
    }
    let (key, value) = match split_yaml_kv(t) {
        Some(pair) => pair,
        None => return AllowEntry::Other,
    };
    AllowEntry::Value(key, value.to_owned())
}

fn split_yaml_kv(t: &str) -> Option<(String, &str)> {
    let (name, rest) = if let Some(r) = t.strip_prefix('"').or_else(|| t.strip_prefix('\'')) {
        let quote = t.chars().next()?;
        let end = r.find(quote)?;
        (r[..end].to_string(), r[end + 1..].trim_start())
    } else {
        let idx = t.find(':')?;
        (t[..idx].trim().to_string(), t[idx..].trim_start())
    };
    let rest = rest.strip_prefix(':')?.trim();
    if name.is_empty() {
        None
    } else {
        Some((name, rest))
    }
}

#[derive(Serialize, Clone)]
pub(crate) struct AllowBuildsChange {
    package: String,
    before: Option<String>,
    after: Option<String>,
}

fn allow_builds_changes(head: &AllowSplit, working: &AllowSplit) -> Vec<AllowBuildsChange> {
    let names: std::collections::BTreeSet<_> =
        head.entries.keys().chain(working.entries.keys()).collect();
    names
        .into_iter()
        .filter_map(|name| {
            let before = head.entries.get(name).cloned();
            let after = working.entries.get(name).cloned();
            (before != after).then(|| AllowBuildsChange {
                package: name.clone(),
                before,
                after,
            })
        })
        .collect()
}

fn classify_workspace_file(dir: &Path) -> Result<(bool, Vec<AllowBuildsChange>), String> {
    let (ok, root, err) = run_capture(dir, "git rev-parse --show-toplevel")?;
    if !ok {
        return Err(format!("读取 Git 根目录失败: {}", err.trim()));
    }
    let root = Path::new(root.trim_end());
    let working = match std::fs::read_to_string(root.join("pnpm-workspace.yaml")) {
        Ok(content) => content,
        Err(_) => return Ok((false, Vec::new())),
    };
    let (ok, out, _) = run_capture(root, "git show HEAD:pnpm-workspace.yaml")?;
    let base = if ok { out } else { String::new() };
    let head = split_allow_builds(&base);
    let working = split_allow_builds(&working);
    let (index_exists, index, _) = run_capture(root, "git show :pnpm-workspace.yaml")?;
    let staged = split_allow_builds(if index_exists { &index } else { "" });
    let mut changes = allow_builds_changes(&head, &working);
    if changes.is_empty() {
        changes = allow_builds_changes(&head, &staged);
    }
    let other_changes =
        head.rest.trim() != working.rest.trim() || head.rest.trim() != staged.rest.trim();
    Ok((!other_changes && !changes.is_empty(), changes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn write_package_version(path: &Path, version: &str) -> Result<String, String> {
        let original = read_regular_file(path)?;
        let previous = package_version(&original)?;
        let updated = update_package_version(&original, version)?;
        let edit = ReleaseFileEdit::new(path.to_path_buf(), original, updated);
        with_release_file_edits(&[edit], || Ok(previous)).map_err(|(_, error)| error)
    }

    #[test]
    fn bump_updates_only_the_top_level_version_even_when_a_nested_value_matches() {
        let path = std::env::temp_dir().join(format!(
            "fpm-package-version-{}.json",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let original =
            "{\r\n  \"metadata\": {\"version\": \"1.0.54\"},\r\n  \"version\": \"1.0.54\"\r\n}";
        fs::write(&path, original).unwrap();
        assert_eq!(write_package_version(&path, "1.0.55").unwrap(), "1.0.54");
        let actual = fs::read_to_string(&path).unwrap();
        fs::remove_file(&path).unwrap();
        assert_eq!(
            actual,
            original.replace(
                "  \"version\": \"1.0.54\"\r\n",
                "  \"version\": \"1.0.55\"\r\n"
            )
        );
    }

    #[test]
    fn applies_both_release_files_and_restores_them_on_build_or_write_failure() {
        let repo = std::env::temp_dir().join(format!(
            "fpm-announcement-{}",
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        fs::create_dir_all(repo.join("src/config")).unwrap();
        let package = repo.join("package.json");
        let log = repo.join("src/config/versionLog.json");
        let original_package = "{\n  \"version\": \"1.0.54\"\n}";
        let original_log = r#"[{"version":"1.0.54","date":"2026-11-24","list":["旧公告"]}]"#;
        fs::write(&package, original_package).unwrap();
        fs::write(&log, original_log).unwrap();
        for command in ["git init -q -b master", "git add -A", "git -c user.name=test -c user.email=test@example.test -c commit.gpgsign=false commit -qm initial"] {
            let (ok, _, error) = run_capture(&repo, command).unwrap();
            assert!(ok, "{command}: {error}");
        }
        let context = read_release_context(
            repo.to_string_lossy().into_owned(),
            "master".into(),
            None,
            None,
        )
        .unwrap();
        let preview = serde_json::to_value(context.version_log.unwrap()).unwrap();
        assert_eq!(preview["latest"]["version"], "1.0.54");
        assert_eq!(
            prepare_release_edits(&repo, Some(""), "1.0.55", "1.0.54", None)
                .unwrap()
                .len(),
            1
        );
        let mut draft = ReleaseAnnouncement {
            log_revision: "stale".into(),
            date: "2026-11-25".into(),
            list: vec!["修正日期".into()],
        };
        assert!(prepare_release_edits(&repo, None, "1.0.55", "1.0.54", Some(&draft)).is_err());
        draft.log_revision = preview["revision"].as_str().unwrap().into();
        let edits = prepare_release_edits(&repo, None, "1.0.55", "1.0.54", Some(&draft)).unwrap();
        let error = with_release_file_edits(&edits, || {
            assert_eq!(
                package_version(&fs::read_to_string(&package).unwrap()).unwrap(),
                "1.0.55"
            );
            let contents: serde_json::Value =
                serde_json::from_str(&fs::read_to_string(&log).unwrap()).unwrap();
            assert_eq!(contents[1]["version"], "1.0.55");
            assert_eq!(contents[1]["list"][0], "修正日期");
            Err::<(), _>(("build", "构建失败".into()))
        })
        .unwrap_err();
        assert_eq!(error.0, "build");
        assert_eq!(fs::read_to_string(&package).unwrap(), original_package);
        assert_eq!(fs::read_to_string(&log).unwrap(), original_log);

        fs::write(
            log.with_file_name("versionLog.json.fpm-release.tmp"),
            "已有的临时文件",
        )
        .unwrap();
        let error = with_release_file_edits::<()>(&edits, || panic!("写入失败后不能构建"))
            .unwrap_err();
        assert_eq!(error.0, "bump");
        assert_eq!(fs::read_to_string(&package).unwrap(), original_package);
        assert_eq!(fs::read_to_string(&log).unwrap(), original_log);
        fs::remove_dir_all(repo).unwrap();
    }

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
        assert_eq!(
            sanitize_filename("a/b\\c:d*e?f\"g<h>i|j"),
            "a-b-c-d-e-f-g-h-i-j"
        );
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
        assert_eq!(
            package_version("{\"version\": \"1.0.0\"}").unwrap(),
            "1.0.0"
        );
        assert!(package_version("{\"name\": \"demo\"}").is_err());
        assert!(package_version("not json").is_err());
    }

    /// 版本基准必须来自发布分支的已提交内容，而不是工作区当前检出的那份。
    /// 建真仓库跑：master 是 1.4.0，切到 feature 后改到 2.1.0，读 master 应得 1.4.0。
    #[test]
    fn release_context_reads_base_from_release_branch() {
        // 需要 git；环境里没有就跳过（这条流水线本身也依赖 git）
        if !matches!(
            run_capture(Path::new("."), "git --version"),
            Ok((true, _, _))
        ) {
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
        let ctx = read_release_context(path.clone(), "master".to_string(), None, None).unwrap();
        assert_eq!(ctx.release_version, "1.4.0", "基准应取发布分支的已提交版本");
        assert_eq!(ctx.release_ref, "master");
        assert_eq!(ctx.current_branch.as_deref(), Some("feature"));
        assert_eq!(ctx.working_version.as_deref(), Some("2.1.0"));
        assert!(!ctx.dirty);

        // 工作区脏（上次发布后版本号没提交）也能读出来，供弹窗提示
        fs::write(repo.join("package.json"), "{\"version\": \"2.1.1\"}").unwrap();
        let ctx = read_release_context(path, "master".to_string(), None, None).unwrap();
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
        let project = dist.parent().unwrap();
        let before = snapshot(project, Path::new("dist")).unwrap();
        fs::write(product.join("index.html"), "new build contents").unwrap();
        let after = snapshot(project, Path::new("dist")).unwrap();
        let artifacts = select_artifacts(&dist, &before, after, "1.0.0").unwrap();
        write_archive(&artifacts, &zip_path, ZIP_LEVEL).unwrap();

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
        let after = snapshot(dist.parent().unwrap(), Path::new("dist")).unwrap();
        let empty = snapshot(dist.parent().unwrap(), Path::new("does-not-exist")).unwrap();
        let artifacts = select_artifacts(&dist, &empty, after, "1.0.0").unwrap();
        write_archive(&artifacts, &zip_path, ZIP_LEVEL).unwrap();

        let f = fs::File::open(&zip_path).unwrap();
        let archive = zip::ZipArchive::new(f).unwrap();
        let mut names: Vec<String> = archive.file_names().map(str::to_string).collect();
        names.sort();
        assert_eq!(names, vec!["assets/", "assets/app.js", "index.html"]);
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn ignore_matches_env_development_and_user_patterns() {
        assert!(path_ignored(".env.development", &[]));
        assert!(path_ignored("apps/web/.env.development", &[]));
        assert!(!path_ignored(".env.local", &[]));
        assert!(path_ignored(".env.local", &[".env.local".to_string()]));
        assert!(path_ignored(
            "packages/app/.env.local",
            &["*.local".to_string()]
        ));
        assert!(path_ignored(
            "coverage/lcov.info",
            &["coverage/".to_string()]
        ));
        assert!(!path_ignored("src/main.ts", &["*.local".to_string()]));
        // 过宽的模式等于关掉预检，不当忽略
        assert!(!path_ignored("src/main.ts", &["**".to_string()]));
        assert!(!path_ignored("src/main.ts", &["*".to_string()]));
    }

    #[test]
    fn porcelain_parser_takes_rename_destination_and_unquotes() {
        let out = " M src/a.ts\n?? .env.development\nR  \"old file.ts\" -> \"new file.ts\"\n";
        assert_eq!(
            parse_porcelain_paths(out),
            vec![
                "src/a.ts".to_string(),
                ".env.development".to_string(),
                "new file.ts".to_string(),
            ]
        );
    }

    #[test]
    fn allow_builds_diff_tracks_add_remove_and_unresolved_values() {
        let head = split_allow_builds(
            "packages:\n  - apps/*\nallowBuilds:\n  esbuild: false\n  removed: true\n",
        );
        let working = split_allow_builds("packages:\n  - apps/*\nallowBuilds:\n  esbuild: true\n  added: set this to true or false\n");
        assert_eq!(head.rest, working.rest);
        let changes = allow_builds_changes(&head, &working);
        assert!(changes.iter().any(|entry| entry.package == "esbuild"
            && entry.before.as_deref() == Some("false")
            && entry.after.as_deref() == Some("true")));
        assert!(changes
            .iter()
            .any(|entry| entry.package == "removed" && entry.after.is_none()));
        assert!(changes.iter().any(|entry| entry.package == "added"
            && entry.after.as_deref() == Some("set this to true or false")));
    }

    #[test]
    fn workspace_status_ignores_env_and_allow_builds() {
        if !matches!(
            run_capture(Path::new("."), "git --version"),
            Ok((true, _, _))
        ) {
            return;
        }
        let base = std::env::temp_dir().join(format!(
            "fpm-ws-test-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let repo = base.join("repo");
        fs::create_dir_all(&repo).unwrap();
        let git = |cmd: &str| {
            let full = format!("git -c user.email=t@t -c user.name=t {cmd}");
            let (ok, _, err) = run_capture(&repo, &full).unwrap();
            assert!(ok, "`{full}` 失败: {}", err.trim());
        };
        git("init -q");
        fs::write(repo.join("package.json"), "{\"version\": \"1.0.0\"}").unwrap();
        fs::write(
            repo.join("pnpm-workspace.yaml"),
            "packages:\n  - 'packages/*'\n",
        )
        .unwrap();
        git("add -A");
        git("commit -q -m init");

        fs::write(repo.join(".env.development"), "VITE_X=1\n").unwrap();
        fs::create_dir_all(repo.join("apps")).unwrap();
        fs::write(repo.join("apps/.env.development"), "VITE_Y=1\n").unwrap();
        fs::write(
            repo.join("pnpm-workspace.yaml"),
            "packages:\n  - 'packages/*'\nallowBuilds:\n  esbuild: true\n  '@parcel/watcher': true\n  vue-demi: true\n",
        )
        .unwrap();

        let ws = workspace_status(&repo, &[]).unwrap();
        assert!(ws.blocking.is_empty(), "blocking = {:?}", ws.blocking);
        assert!(ws.ignored.iter().any(|p| p == ".env.development"));
        assert!(ws.ignored.iter().any(|p| p.ends_with(".env.development")));
        assert!(ws.allow_builds_ignored);
        assert_eq!(ws.allow_builds_changes.len(), 3);

        git("add pnpm-workspace.yaml");
        let (_, before, _) =
            run_capture(&repo, "git diff --cached -- pnpm-workspace.yaml").unwrap();
        let _ = workspace_status(&repo, &[]).unwrap();
        let (_, after, _) = run_capture(&repo, "git diff --cached -- pnpm-workspace.yaml").unwrap();
        assert!(!before.is_empty());
        assert_eq!(before, after);
        assert!(fs::read_to_string(repo.join("pnpm-workspace.yaml"))
            .unwrap()
            .contains("esbuild: true"));

        // 真正的源码改动仍然拦住，忽略项继续不算
        fs::write(repo.join("package.json"), "{\"version\": \"1.0.1\"}").unwrap();
        let ws = workspace_status(&repo, &[]).unwrap();
        assert_eq!(ws.blocking, vec!["package.json".to_string()]);
        assert!(ws.allow_builds_ignored);

        let _ = fs::remove_dir_all(&base);
    }
}
