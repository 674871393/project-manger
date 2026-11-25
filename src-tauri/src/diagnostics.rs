use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::Serialize;
use serde_json::Value;
use tauri::{AppHandle, Manager};

use crate::commands::Project;
use crate::ports::{describe_listeners, PortOwner};
use crate::process_manager::{validate_package_manager, ProcessManager};

fn package_json(dir: &Path) -> Option<Value> {
    serde_json::from_slice(&fs::read(dir.join("package.json")).ok()?).ok()
}

fn project_ancestors(dir: &Path) -> impl Iterator<Item = &Path> {
    let git_root = dir.ancestors().find(|parent| parent.join(".git").exists());
    dir.ancestors()
        .take_while(move |parent| git_root.is_none_or(|root| parent.starts_with(root)))
}

fn is_workspace_root(dir: &Path) -> bool {
    dir.join("pnpm-workspace.yaml").is_file()
        || package_json(dir).is_some_and(|pkg| pkg.get("workspaces").is_some())
}

fn workspace_root(dir: &Path) -> &Path {
    project_ancestors(dir)
        .find(|parent| is_workspace_root(parent))
        .unwrap_or(dir)
}

pub(crate) fn package_manager_for(dir: &Path, package: &Value) -> String {
    let root = workspace_root(dir);
    let declared = |pkg: &Value| -> Option<String> {
        let name = pkg["packageManager"].as_str()?.split('@').next()?;
        validate_package_manager(name).ok()?;
        Some(name.to_owned())
    };
    if let Some(name) = declared(package).or_else(|| package_json(root).as_ref().and_then(declared))
    {
        return name;
    }
    for dir in [dir, root] {
        for (file, manager) in [
            ("pnpm-lock.yaml", "pnpm"),
            ("yarn.lock", "yarn"),
            ("bun.lock", "bun"),
            ("bun.lockb", "bun"),
            ("package-lock.json", "npm"),
        ] {
            if dir.join(file).is_file() {
                return manager.into();
            }
        }
    }
    if root.join("pnpm-workspace.yaml").is_file() {
        "pnpm".into()
    } else {
        "npm".into()
    }
}

pub(crate) fn dependencies_available(dir: &Path) -> bool {
    let root = workspace_root(dir);
    [dir, root].iter().any(|path| {
        path.join("node_modules").is_dir()
            || path.join(".pnp.cjs").is_file()
            || path.join(".pnp.js").is_file()
    }) || package_json(dir).is_some_and(|pkg| {
        [
            "dependencies",
            "devDependencies",
            "optionalDependencies",
            "peerDependencies",
        ]
        .iter()
        .all(|key| pkg[*key].as_object().is_none_or(|deps| deps.is_empty()))
    })
}

pub(crate) fn git_head(dir: &Path) -> Result<Option<String>, String> {
    if !dir.is_dir() {
        return Ok(None);
    }
    let Some((root, dot_git)) = dir.ancestors().find_map(|parent| {
        let git = parent.join(".git");
        git.exists().then_some((parent, git))
    }) else {
        return Ok(None);
    };
    let git_dir = if dot_git.is_file() {
        let pointer =
            fs::read_to_string(&dot_git).map_err(|e| format!("读取 Git 指针失败: {e}"))?;
        let Some(git_dir) = pointer.trim().strip_prefix("gitdir:") else {
            return Ok(None);
        };
        root.join(git_dir.trim())
    } else {
        dot_git
    };
    let head =
        fs::read_to_string(git_dir.join("HEAD")).map_err(|e| format!("读取 Git HEAD 失败: {e}"))?;
    let head = head.trim();
    if let Some(branch) = head.strip_prefix("ref: refs/heads/") {
        return Ok(Some(branch.into()));
    }
    Ok(
        (head.len() >= 7 && head.bytes().all(|byte| byte.is_ascii_hexdigit()))
            .then(|| head[..7].into()),
    )
}

pub(crate) fn executable_on_path(name: &str) -> Option<PathBuf> {
    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths)
        .filter(|dir| dir.is_absolute())
        .find_map(|dir| {
            #[cfg(windows)]
            let names = [
                format!("{name}.exe"),
                format!("{name}.cmd"),
                format!("{name}.bat"),
            ];
            #[cfg(not(windows))]
            let names = [name.to_owned()];
            names
                .into_iter()
                .map(|name| dir.join(name))
                .find(|path| path.is_file())
        })
}

pub(crate) fn hide_console(command: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(crate::process_manager::CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    let _ = command;
}

fn installed_version(program: &str, cwd: &Path) -> Result<String, String> {
    let path = executable_on_path(program).ok_or_else(|| format!("PATH 中找不到 {program}"))?;
    let mut command = Command::new(path);
    command
        .arg("--version")
        .current_dir(cwd)
        .stdin(Stdio::null())
        .env("COREPACK_ENABLE_NETWORK", "0")
        .env("COREPACK_ENABLE_DOWNLOAD_PROMPT", "0");
    hide_console(&mut command);
    let out = command
        .output()
        .map_err(|e| format!("执行 {program} 失败: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
    } else {
        let error = String::from_utf8_lossy(&out.stderr);
        Err(error
            .lines()
            .find(|line| !line.trim().is_empty())
            .unwrap_or("版本查询失败")
            .chars()
            .take(240)
            .collect())
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProjectDiagnostics {
    directory_exists: bool,
    package_exists: bool,
    workspace_root: Option<String>,
    detected_package_manager: Option<String>,
    node: Result<String, String>,
    package_manager: Result<String, String>,
    dependencies_available: bool,
    script_exists: bool,
    git_branch: Option<String>,
}

#[tauri::command]
pub(crate) async fn diagnose_project(project: Project) -> Result<ProjectDiagnostics, String> {
    validate_package_manager(&project.package_manager)?;
    tauri::async_runtime::spawn_blocking(move || {
        let dir = Path::new(&project.path);
        let root = workspace_root(dir);
        let package = package_json(dir);
        let tool_dir = std::env::current_exe()
            .ok()
            .and_then(|path| path.parent().map(Path::to_path_buf))
            .unwrap_or_else(std::env::temp_dir);
        ProjectDiagnostics {
            directory_exists: dir.is_dir(),
            package_exists: package.is_some(),
            workspace_root: is_workspace_root(root).then(|| root.to_string_lossy().into_owned()),
            detected_package_manager: package.as_ref().map(|pkg| package_manager_for(dir, pkg)),
            node: installed_version("node", &tool_dir),
            package_manager: installed_version(&project.package_manager, &tool_dir),
            dependencies_available: dependencies_available(dir),
            script_exists: package
                .as_ref()
                .and_then(|pkg| pkg.get("scripts"))
                .is_some_and(|scripts| {
                    scripts.get(&project.script).is_some()
                        || project
                            .script
                            .split_whitespace()
                            .next()
                            .is_some_and(|name| scripts.get(name).is_some())
                }),
            git_branch: git_head(dir).ok().flatten(),
        }
    })
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub(crate) async fn list_listening_ports(app: AppHandle) -> Result<Vec<PortOwner>, String> {
    let roots = app.state::<ProcessManager>().running_roots();
    tauri::async_runtime::spawn_blocking(move || describe_listeners(&roots))
        .await
        .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_workspace_dependencies_and_git_head_from_a_linked_worktree_member() {
        let base = std::env::temp_dir().join(format!(
            "fpm-worktree-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        let worktree = base.join("worktree");
        let member = worktree.join("apps").join("web");
        fs::create_dir_all(&member).unwrap();
        fs::create_dir_all(base.join("git-data")).unwrap();
        fs::write(worktree.join(".git"), "gitdir: ../git-data\n").unwrap();
        fs::write(
            base.join("git-data").join("HEAD"),
            "ref: refs/heads/feature/demo\n",
        )
        .unwrap();
        fs::write(
            worktree.join("pnpm-workspace.yaml"),
            "packages:\n  - apps/*\n",
        )
        .unwrap();
        fs::write(
            worktree.join("package.json"),
            r#"{"packageManager":"pnpm@11.0.0"}"#,
        )
        .unwrap();
        fs::write(
            member.join("package.json"),
            r#"{"dependencies":{"react":"*"}}"#,
        )
        .unwrap();
        fs::create_dir(worktree.join("node_modules")).unwrap();

        assert_eq!(git_head(&member).unwrap().as_deref(), Some("feature/demo"));
        assert_eq!(workspace_root(&member), worktree);
        assert_eq!(
            package_manager_for(&member, &package_json(&member).unwrap()),
            "pnpm"
        );
        assert!(dependencies_available(&member));
        fs::remove_dir_all(base).unwrap();
    }
}
