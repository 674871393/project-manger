use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::commands::Project;
use crate::diagnostics::{executable_on_path, hide_console};

pub(crate) fn validate_node_version(version: &str) -> bool {
    let parts: Vec<_> = version.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
}

fn node_version(path: &Path) -> Result<String, String> {
    let mut command = Command::new(path);
    command.arg("--version").stdin(Stdio::null());
    hide_console(&mut command);
    let output = command
        .output()
        .map_err(|error| format!("无法运行 {}: {error}", path.display()))?;
    if !output.status.success() {
        return Err(format!("无法读取 {} 的 Node 版本", path.display()));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .trim()
        .trim_start_matches('v')
        .to_owned())
}

pub(crate) fn resolve_node(version: &str) -> Result<PathBuf, String> {
    let file = if cfg!(windows) { "node.exe" } else { "node" };
    let installed = std::env::var_os("NVM_HOME")
        .map(|root| PathBuf::from(root).join(format!("v{version}")).join(file));
    let node = installed
        .filter(|path| path.is_file())
        .or_else(|| executable_on_path("node"))
        .ok_or_else(|| format!("找不到 Node {version}，请先用 nvm 安装该版本"))?;
    let node = node
        .canonicalize()
        .map_err(|error| format!("定位 Node 失败: {error}"))?;
    let actual = node_version(&node)?;
    if actual != version {
        return Err(format!(
            "找不到 Node {version}（当前为 {actual}）。请用 nvm 安装目标版本，或检查 NVM_HOME"
        ));
    }
    Ok(node)
}

fn package_entry(dir: &Path, name: &str) -> Option<PathBuf> {
    let package_dir = dir.join("node_modules").join(name);
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(package_dir.join("package.json")).ok()?).ok()?;
    let bin = manifest["bin"]
        .as_str()
        .or_else(|| manifest["bin"][name].as_str())?;
    let entry = package_dir.join(bin);
    entry.is_file().then_some(entry)
}

fn manager_command(node: &Path, node_dir: &Path, name: &str) -> Result<Command, String> {
    let installed = executable_on_path(name);
    if name == "bun" {
        return installed
            .map(Command::new)
            .ok_or_else(|| "PATH 中找不到 Bun".into());
    }
    let entry = package_entry(node_dir, name).or_else(|| {
        let dir = installed.as_deref()?.parent()?;
        package_entry(dir, name)
    });
    if let Some(entry) = entry {
        let mut command = Command::new(node);
        command.arg(entry);
        return Ok(command);
    }

    let corepack = node_dir
        .join("node_modules")
        .join("corepack")
        .join("dist")
        .join(format!("{name}.js"));
    if name != "npm" && corepack.is_file() {
        let mut command = Command::new(node);
        command.arg(corepack).env("COREPACK_ENABLE_NETWORK", "0");
        return Ok(command);
    }
    if let Some(exe) = installed.filter(|path| {
        path.extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("exe"))
    }) {
        return Ok(Command::new(exe));
    }
    Err(format!(
        "找不到可在指定 Node 下运行的 {name}。请在该 Node 版本下安装包管理器"
    ))
}

pub(crate) fn build_command(
    project: &Project,
    node: &Path,
    script: &str,
) -> Result<Command, String> {
    let node_dir = node.parent().ok_or("Node 安装目录无效")?;
    let mut command = manager_command(node, node_dir, &project.package_manager)?;
    let mut path = std::env::var_os("PATH").unwrap_or_default();
    if let Some(env) = &project.env {
        for (key, value) in env {
            if key.eq_ignore_ascii_case("PATH") {
                path = OsString::from(value);
            } else {
                command.env(key, value);
            }
        }
    }
    let path = std::env::join_paths(
        std::iter::once(node_dir.to_path_buf()).chain(std::env::split_paths(&path)),
    )
    .map_err(|error| format!("设置本次构建 PATH 失败: {error}"))?;
    command
        .env("PATH", path)
        .args(["run", script])
        .current_dir(&project.path);
    Ok(command)
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn build_uses_its_own_node_and_leaves_the_current_version_alone() {
        let Some(default_node) = executable_on_path("node") else {
            return;
        };
        let current = node_version(&default_node).unwrap();
        let version = std::env::var_os("NVM_HOME")
            .and_then(|path| {
                std::fs::read_dir(path)
                    .ok()?
                    .filter_map(Result::ok)
                    .filter_map(|entry| {
                        let name = entry.file_name().into_string().ok()?;
                        let version = name.strip_prefix('v')?;
                        if version != current
                            && validate_node_version(version)
                            && entry.path().join("node.exe").is_file()
                            && package_entry(&entry.path(), "npm").is_some()
                        {
                            Some(version.to_owned())
                        } else {
                            None
                        }
                    })
                    .next()
            })
            .unwrap_or_else(|| current.clone());
        let selected = resolve_node(&version).unwrap();
        if package_entry(selected.parent().unwrap(), "npm").is_none() {
            return;
        }
        let dir = std::env::temp_dir().join(format!(
            "fpm-node-test-{}",
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(
            dir.join("package.json"),
            r#"{"scripts":{"test-build":"node check.cjs"}}"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("check.cjs"),
            "process.stdout.write(process.version)",
        )
        .unwrap();
        let project = Project {
            id: "test".into(),
            name: "test".into(),
            path: dir.to_string_lossy().into_owned(),
            package_manager: "npm".into(),
            script: "dev".into(),
            manual_port: None,
            env: None,
            framework: None,
            release: None,
            created_at: 0,
        };
        let result = build_command(&project, &selected, "test-build")
            .unwrap()
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(String::from_utf8_lossy(&result.stdout)
            .trim_end()
            .ends_with(&format!("v{version}")));
        assert_eq!(node_version(&default_node).unwrap(), current);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
