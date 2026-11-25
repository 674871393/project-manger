use std::fs;
use std::io::Write;
use std::ops::Range;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::value::RawValue;

pub(crate) type PipelineError = (&'static str, String);

#[derive(Deserialize)]
struct PackageVersion<'a> {
    #[serde(borrow)]
    version: &'a RawValue,
}

fn package_version_token(content: &str) -> Result<&RawValue, String> {
    serde_json::from_str::<PackageVersion<'_>>(content)
        .map(|package| package.version)
        .map_err(|error| format!("读取 package.json 顶层 version 失败: {error}"))
}

pub(crate) fn package_version(content: &str) -> Result<String, String> {
    serde_json::from_str(package_version_token(content)?.get())
        .map_err(|error| format!("package.json 顶层 version 应为字符串: {error}"))
}

pub(crate) fn token_range(document: &str, token: &RawValue) -> Range<usize> {
    let value = token.get();
    let start = value.as_ptr() as usize - document.as_ptr() as usize;
    start..start + value.len()
}

pub(crate) fn update_package_version(content: &str, next: &str) -> Result<String, String> {
    let token = package_version_token(content)?;
    let previous: String = serde_json::from_str(token.get()).map_err(|e| e.to_string())?;
    if previous == next {
        return Ok(content.to_owned());
    }
    let mut updated = content.to_owned();
    let value = serde_json::to_string(next).map_err(|error| error.to_string())?;
    updated.replace_range(token_range(content, token), &value);
    Ok(updated)
}

pub(crate) struct ReleaseFileEdit {
    path: PathBuf,
    original: String,
    updated: String,
}

impl ReleaseFileEdit {
    pub(crate) fn new(path: PathBuf, original: String, updated: String) -> Self {
        Self {
            path,
            original,
            updated,
        }
    }

    fn apply(&self) -> Result<(), String> {
        if read_regular_file(&self.path)? != self.original {
            return Err(format!("{} 已发生变化，请重新检查", self.path.display()));
        }
        if self.original != self.updated {
            atomic_write(&self.path, &self.updated)?;
        }
        Ok(())
    }

    fn restore(&self) -> Result<(), String> {
        let current = read_regular_file(&self.path)?;
        if current == self.original {
            return Ok(());
        }
        if current != self.updated {
            return Err(format!(
                "{} 已被其他操作修改，已保留当前内容",
                self.path.display()
            ));
        }
        atomic_write(&self.path, &self.original)
    }
}

pub(crate) fn read_regular_file(path: &Path) -> Result<String, String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("读取 {} 失败: {error}", path.display()))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(format!("{} 应为普通文件", path.display()));
    }
    fs::read_to_string(path).map_err(|error| format!("读取 {} 失败: {error}", path.display()))
}

fn atomic_write(path: &Path, content: &str) -> Result<(), String> {
    let mut name = path.file_name().ok_or("版本文件名无效")?.to_os_string();
    name.push(".fpm-release.tmp");
    let temporary = path.with_file_name(name);
    let mut file = fs::File::create_new(&temporary)
        .map_err(|error| format!("为 {} 创建临时文件失败: {error}", path.display()))?;
    let written = file
        .write_all(content.as_bytes())
        .and_then(|_| file.sync_all());
    drop(file);
    let result = written.and_then(|_| fs::rename(&temporary, path));
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result.map_err(|error| format!("写入 {} 失败: {error}", path.display()))
}

fn with_rollback_message(mut error: String, applied: &[&ReleaseFileEdit]) -> String {
    if applied.is_empty() {
        return error;
    }
    let failures: Vec<_> = applied
        .iter()
        .rev()
        .filter_map(|edit| edit.restore().err())
        .collect();
    if failures.is_empty() {
        error.push_str("。本次修改的版本文件已恢复");
    } else {
        error.push_str(&format!("。部分版本文件恢复失败，{}", failures.join("，")));
    }
    error
}

pub(crate) fn with_release_file_edits<T>(
    edits: &[ReleaseFileEdit],
    build: impl FnOnce() -> Result<T, PipelineError>,
) -> Result<T, PipelineError> {
    let mut applied = Vec::new();
    for edit in edits {
        if let Err(error) = edit.apply() {
            return Err(("bump", with_rollback_message(error, &applied)));
        }
        if edit.original != edit.updated {
            applied.push(edit);
        }
    }
    build().map_err(|(step, error)| (step, with_rollback_message(error, &applied)))
}
