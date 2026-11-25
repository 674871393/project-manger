use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, Metadata};
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;

use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

#[derive(PartialEq, Eq)]
struct FileStamp {
    size: u64,
    modified: Option<SystemTime>,
    created: Option<SystemTime>,
}

pub(crate) struct ArtifactSnapshot(BTreeMap<PathBuf, FileStamp>);

pub(crate) struct Artifacts {
    pub(crate) root: PathBuf,
    files: Vec<PathBuf>,
}

pub(crate) struct ZipStats {
    pub(crate) files: usize,
    pub(crate) raw_bytes: u64,
    pub(crate) packed_bytes: u64,
}

pub(crate) fn validate_output_dir(value: &str) -> Result<&Path, String> {
    let path = Path::new(value);
    if path.as_os_str().is_empty()
        || !path.components().all(|part| match part {
            Component::Normal(name) => {
                let name = name.to_string_lossy();
                !name.contains(['<', '>', ':', '"', '|', '?', '*', '\0'])
                    && !name.ends_with(['.', ' '])
            }
            _ => false,
        })
    {
        return Err("产物目录须为项目下的相对路径，例如 dist 或 .output/public".into());
    }
    Ok(path)
}

fn is_link(metadata: &Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    metadata.file_type().is_symlink()
}

fn is_archive(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| {
            let name = name.to_ascii_lowercase();
            name.ends_with(".zip") || name.ends_with(".zip.part")
        })
}

fn read_files(
    root: &Path,
    current: &Path,
    files: &mut BTreeMap<PathBuf, FileStamp>,
) -> io::Result<()> {
    for entry in fs::read_dir(current)? {
        let path = entry?.path();
        if is_archive(&path) {
            continue;
        }
        let metadata = fs::symlink_metadata(&path)?;
        if is_link(&metadata) {
            return Err(io::Error::other(format!(
                "产物包含符号链接或目录联接: {}",
                path.display()
            )));
        }
        if metadata.is_dir() {
            read_files(root, &path, files)?;
        } else if metadata.is_file() {
            let relative = path.strip_prefix(root).map_err(io::Error::other)?;
            files.insert(
                relative.to_path_buf(),
                FileStamp {
                    size: metadata.len(),
                    modified: metadata.modified().ok(),
                    created: metadata.created().ok(),
                },
            );
        }
    }
    Ok(())
}

pub(crate) fn snapshot(project: &Path, output: &Path) -> Result<ArtifactSnapshot, String> {
    let mut root = project.to_path_buf();
    for part in output.components() {
        root.push(part);
        match fs::symlink_metadata(&root) {
            Ok(metadata) if metadata.is_dir() && !is_link(&metadata) => {}
            Ok(_) => return Err(format!("产物路径不是普通目录: {}", root.display())),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(ArtifactSnapshot(BTreeMap::new()))
            }
            Err(error) => return Err(format!("读取产物路径失败: {error}")),
        }
    }
    let mut files = BTreeMap::new();
    read_files(&root, &root, &mut files).map_err(|e| format!("读取产物文件失败: {e}"))?;
    Ok(ArtifactSnapshot(files))
}

pub(crate) fn select_artifacts(
    output: &Path,
    before: &ArtifactSnapshot,
    after: ArtifactSnapshot,
    version: &str,
) -> Result<Artifacts, String> {
    let changed: Vec<_> = after
        .0
        .iter()
        .filter(|(path, stamp)| before.0.get(*path) != Some(*stamp))
        .map(|(path, _)| path)
        .collect();
    if changed.is_empty() {
        return Err("本次构建没有生成或更新产物，请检查构建脚本和产物目录".into());
    }

    let versioned_dirs: Option<BTreeSet<&str>> = changed
        .iter()
        .map(|path| {
            if path.components().count() < 2 {
                return None;
            }
            path.iter()
                .next()?
                .to_str()
                .filter(|name| name.starts_with("dist-"))
        })
        .collect();
    let folder = match versioned_dirs {
        Some(dirs) if dirs.len() == 1 => dirs.into_iter().next().map(str::to_owned),
        Some(dirs) => {
            let suffix = format!("-{version}");
            let matching: Vec<_> = dirs
                .into_iter()
                .filter(|dir| dir.ends_with(&suffix))
                .collect();
            match matching.as_slice() {
                [dir] => Some((*dir).to_owned()),
                _ => return Err("本次构建更新了多个版本目录，请设置明确的产物目录".into()),
            }
        }
        None => None,
    };
    let prefix = folder
        .as_deref()
        .map(Path::new)
        .unwrap_or_else(|| Path::new(""));
    let files = after
        .0
        .into_keys()
        .filter_map(|path| path.strip_prefix(prefix).ok().map(Path::to_path_buf))
        .collect();
    Ok(Artifacts {
        root: output.join(prefix),
        files,
    })
}

pub(crate) fn write_archive(
    artifacts: &Artifacts,
    destination: &Path,
    level: i64,
) -> Result<ZipStats, String> {
    let temporary = destination.with_extension("zip.part");
    let output = File::create_new(&temporary).map_err(|e| format!("创建临时 ZIP 失败: {e}"))?;
    let result = (|| -> Result<ZipStats, String> {
        let mut zip = ZipWriter::new(output);
        let options = SimpleFileOptions::default()
            .compression_method(CompressionMethod::Deflated)
            .compression_level(Some(level));
        let directories: BTreeSet<_> = artifacts
            .files
            .iter()
            .flat_map(|path| {
                path.ancestors()
                    .skip(1)
                    .filter(|path| !path.as_os_str().is_empty())
            })
            .collect();
        for dir in directories {
            let name = dir.to_string_lossy().replace('\\', "/") + "/";
            zip.add_directory(name, options)
                .map_err(|e| format!("写入 ZIP 目录失败: {e}"))?;
        }
        let mut raw_bytes = 0;
        for relative in &artifacts.files {
            let path = artifacts.root.join(relative);
            let metadata = fs::symlink_metadata(&path).map_err(|e| format!("读取产物失败: {e}"))?;
            if !metadata.is_file() || is_link(&metadata) {
                return Err(format!("产物发生变化: {}", path.display()));
            }
            let mut file = File::open(&path).map_err(|e| format!("打开产物失败: {e}"))?;
            let name = relative.to_string_lossy().replace('\\', "/");
            zip.start_file(name, options)
                .map_err(|e| format!("写入 ZIP 条目失败: {e}"))?;
            raw_bytes += io::copy(&mut file, &mut zip).map_err(|e| format!("压缩失败: {e}"))?;
        }
        let mut file = zip.finish().map_err(|e| format!("完成 ZIP 失败: {e}"))?;
        file.flush()
            .and_then(|_| file.sync_all())
            .map_err(|e| format!("写入 ZIP 失败: {e}"))?;
        let packed_bytes = file
            .metadata()
            .map_err(|e| format!("读取 ZIP 大小失败: {e}"))?
            .len();
        drop(file);
        if destination.exists() {
            return Err("正式 ZIP 路径已被占用".into());
        }
        fs::rename(&temporary, destination).map_err(|e| format!("保存正式 ZIP 失败: {e}"))?;
        Ok(ZipStats {
            files: artifacts.files.len(),
            raw_bytes,
            packed_bytes,
        })
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_output_is_rejected_even_when_the_build_command_succeeds() {
        let base = std::env::temp_dir().join(format!(
            "fpm-stale-output-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        let relative = validate_output_dir(".output/public").unwrap();
        let output = base.join(relative);
        fs::create_dir_all(&output).unwrap();
        fs::write(output.join("index.html"), "previous release").unwrap();

        let before = snapshot(&base, relative).unwrap();
        let after = snapshot(&base, relative).unwrap();
        assert!(select_artifacts(&output, &before, after, "2.0.0").is_err());

        fs::write(output.join("index.html"), "current release contents").unwrap();
        let after = snapshot(&base, relative).unwrap();
        let artifacts = select_artifacts(&output, &before, after, "2.0.0").unwrap();
        let destination = output.join("release.zip");
        let stats = write_archive(&artifacts, &destination, 6).unwrap();
        assert_eq!(stats.files, 1);
        assert!(!destination.with_extension("zip.part").exists());
        assert!(destination.is_file());
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn archive_failure_leaves_no_final_or_partial_zip() {
        let base = std::env::temp_dir().join(format!(
            "fpm-zip-failure-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        fs::create_dir_all(&base).unwrap();
        let destination = base.join("release.zip");
        let artifacts = Artifacts {
            root: base.clone(),
            files: vec![PathBuf::from("disappeared.txt")],
        };
        assert!(write_archive(&artifacts, &destination, 6).is_err());
        assert!(!destination.exists());
        assert!(!destination.with_extension("zip.part").exists());
        fs::remove_dir_all(base).unwrap();
    }
}
