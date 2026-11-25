use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::{Command, Output, Stdio};

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use serde_json::{value::RawValue, Value};

use crate::diagnostics::hide_console;
use crate::release_files::{read_regular_file, token_range, ReleaseFileEdit};

const DEFAULT_VERSION_LOG: &str = "src/config/versionLog.json";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReleaseAnnouncement {
    pub(crate) log_revision: String,
    pub(crate) date: String,
    pub(crate) list: Vec<String>,
}

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
struct VersionLogEntry {
    version: String,
    date: String,
    list: Vec<String>,
    #[serde(flatten)]
    extra: BTreeMap<String, Value>,
}

#[derive(Clone, Serialize)]
pub(crate) struct VersionLogPreview {
    path: String,
    revision: String,
    latest: Option<VersionLogEntry>,
    versions: Vec<String>,
}

pub(crate) struct VersionLogSource {
    path: String,
    revision: String,
    content: String,
}

pub(crate) fn normalize_version_log_path(value: &str) -> Result<String, String> {
    let path = value.trim().replace('\\', "/");
    let valid = path.to_ascii_lowercase().ends_with(".json")
        && path.split('/').all(|part| {
            !part.is_empty()
                && !part.ends_with(['.', ' '])
                && !part.chars().any(|ch| {
                    ch.is_control() || matches!(ch, '<' | '>' | ':' | '"' | '|' | '?' | '*')
                })
        });
    if !valid || path.eq_ignore_ascii_case("package.json") {
        return Err("版本日志须为项目内的 JSON 文件，例如 src/config/versionLog.json".into());
    }
    Ok(path)
}

fn git_output(dir: &Path, args: &[&str]) -> Result<Output, String> {
    let mut command = Command::new("git");
    command.args(args).current_dir(dir).stdin(Stdio::null());
    hide_console(&mut command);
    command
        .output()
        .map_err(|error| format!("读取版本日志的 Git 数据失败: {error}"))
}

pub(crate) fn version_log_at_ref(
    dir: &Path,
    reference: &str,
    configured: Option<&str>,
) -> Result<Option<VersionLogSource>, String> {
    let path = match configured {
        Some("") => return Ok(None),
        Some(path) => normalize_version_log_path(path)?,
        None => DEFAULT_VERSION_LOG.into(),
    };
    let object = format!("{reference}:./{path}");
    let lookup = git_output(dir, &["rev-parse", "--verify", "--end-of-options", &object])?;
    if !lookup.status.success() {
        return if configured.is_some() {
            Err(format!("发布分支 {reference} 上找不到版本日志 {path}"))
        } else {
            Ok(None)
        };
    }
    let revision = String::from_utf8_lossy(&lookup.stdout).trim().to_string();
    let blob = git_output(dir, &["cat-file", "blob", &revision])?;
    if !blob.status.success() {
        return Err(format!("无法读取发布分支上的 {path}"));
    }
    let content =
        String::from_utf8(blob.stdout).map_err(|_| format!("{path} 须使用 UTF-8 编码"))?;
    Ok(Some(VersionLogSource {
        path,
        revision,
        content,
    }))
}

impl VersionLogSource {
    pub(crate) fn preview(self) -> Result<VersionLogPreview, String> {
        let (_, mut entries) =
            parse_entries(&self.content).map_err(|error| format!("{}：{error}", self.path))?;
        let versions = entries.iter().map(|entry| entry.version.clone()).collect();
        Ok(VersionLogPreview {
            path: self.path,
            revision: self.revision,
            latest: entries.pop(),
            versions,
        })
    }

    pub(crate) fn prepare_edit(
        &self,
        dir: &Path,
        version: &str,
        draft: &ReleaseAnnouncement,
    ) -> Result<ReleaseFileEdit, String> {
        if self.revision != draft.log_revision {
            return Err("发布分支的版本公告已变化，请重新检查后再发布".into());
        }
        let project = dir
            .canonicalize()
            .map_err(|e| format!("定位项目目录失败: {e}"))?;
        let path = self
            .path
            .split('/')
            .fold(project.clone(), |dir, part| dir.join(part));
        let target = path
            .canonicalize()
            .map_err(|e| format!("定位 {} 失败: {e}", self.path))?;
        if !target.starts_with(&project) || target == project.join("package.json") {
            return Err("版本日志必须是项目内独立的普通文件".into());
        }
        let original = read_regular_file(&path)?;
        if original.replace("\r\n", "\n") != self.content.replace("\r\n", "\n") {
            return Err(format!(
                "{} 有未提交改动，请先确认并提交，再重新检查",
                self.path
            ));
        }
        let updated = write_announcement(&original, version, draft)
            .map_err(|error| format!("{}：{error}", self.path))?;
        Ok(ReleaseFileEdit::new(path, original, updated))
    }
}

fn parse_entries(content: &str) -> Result<(Vec<&RawValue>, Vec<VersionLogEntry>), String> {
    let raw: Vec<&RawValue> =
        serde_json::from_str(content).map_err(|e| format!("版本日志须为 JSON 数组: {e}"))?;
    let mut seen = BTreeSet::new();
    let mut entries = Vec::with_capacity(raw.len());
    for (index, value) in raw.iter().enumerate() {
        let entry: VersionLogEntry = serde_json::from_str(value.get()).map_err(|e| {
            format!(
                "第 {} 条公告缺少有效的 version、date 或 list: {e}",
                index + 1
            )
        })?;
        if entry.version.trim().is_empty() || !seen.insert(entry.version.clone()) {
            return Err(format!("第 {} 条公告的版本为空或重复", index + 1));
        }
        entries.push(entry);
    }
    Ok((raw, entries))
}

fn write_announcement(
    content: &str,
    version: &str,
    draft: &ReleaseAnnouncement,
) -> Result<String, String> {
    let date = NaiveDate::parse_from_str(&draft.date, "%Y-%m-%d")
        .map_err(|_| "公告日期无效，请使用 YYYY-MM-DD")?;
    if draft.date != date.format("%Y-%m-%d").to_string() {
        return Err("公告日期无效，请使用 YYYY-MM-DD".into());
    }
    let list: Vec<_> = draft
        .list
        .iter()
        .map(|line| line.trim())
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect();
    if list.is_empty() {
        return Err("请至少填写一条更新内容".into());
    }

    let (raw, entries) = parse_entries(content)?;
    let latest = entries.last();
    let same_version = latest.is_some_and(|entry| entry.version == version);
    if !same_version && entries.iter().any(|entry| entry.version == version) {
        return Err(format!("版本 {version} 已有历史公告，请选择新版本"));
    }
    let entry = VersionLogEntry {
        version: version.into(),
        date: draft.date.clone(),
        list,
        extra: if same_version {
            latest.map(|value| value.extra.clone()).unwrap_or_default()
        } else {
            BTreeMap::new()
        },
    };
    if same_version && latest == Some(&entry) {
        return Ok(content.into());
    }

    format_announcement(content, raw.last().copied(), &entry, same_version)
}

fn format_announcement(
    content: &str,
    last: Option<&RawValue>,
    entry: &VersionLogEntry,
    replace: bool,
) -> Result<String, String> {
    let newline = if content.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let last_range = last.map(|value| token_range(content, value));
    let indent = last_range
        .as_ref()
        .map(|range| {
            let prefix = content[..range.start].rsplit('\n').next().unwrap_or("");
            if !prefix.is_empty() && prefix.chars().all(|ch| matches!(ch, ' ' | '\t')) {
                prefix
            } else {
                "  "
            }
        })
        .unwrap_or("  ");
    let text = serde_json::to_string_pretty(entry)
        .map_err(|e| format!("生成公告失败: {e}"))?
        .replace('\n', &format!("{newline}{indent}"));
    let mut updated = content.to_owned();
    if let Some(range) = last_range {
        if replace {
            updated.replace_range(range, &text);
        } else {
            updated.insert_str(range.end, &format!(",{newline}{indent}{text}"));
        }
    } else {
        let close = content.trim_end().len() - 1;
        let open = content.find('[').ok_or("版本日志须为 JSON 数组")?;
        updated.replace_range(
            open + 1..close,
            &format!("{newline}{indent}{text}{newline}"),
        );
    }
    Ok(updated)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft(date: &str, notes: &[&str]) -> ReleaseAnnouncement {
        ReleaseAnnouncement {
            log_revision: String::new(),
            date: date.into(),
            list: notes.iter().map(|note| note.to_string()).collect(),
        }
    }

    #[test]
    fn appends_an_entry_without_reformatting_the_existing_log() {
        let original = "[\r\n  {\r\n    \"version\": \"1.0.54\",\r\n    \"date\": \"2026-11-24\",\r\n    \"list\": [\"之前的更新\"]\r\n  }\r\n]";
        let result = write_announcement(
            original,
            "1.0.55",
            &draft("2026-11-25", &["  优化列表  ", "", "修正日期"]),
        )
        .unwrap();
        let old_entry = original.strip_suffix("\r\n]").unwrap();
        assert!(result.starts_with(&format!("{old_entry},\r\n")));
        assert!(!result.replace("\r\n", "").contains('\n'));
        let (_, entries) = parse_entries(&result).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[1].version, "1.0.55");
        assert_eq!(entries[1].list, ["优化列表", "修正日期"]);
    }

    #[test]
    fn reuses_or_updates_the_latest_version_and_preserves_its_other_fields() {
        let original =
            r#"[{"version":"1.0.54","date":"2026-11-24","list":["原公告"],"tag":"stable"}]"#;
        let reused =
            write_announcement(original, "1.0.54", &draft("2026-11-24", &["原公告"])).unwrap();
        assert_eq!(reused, original);
        let updated =
            write_announcement(original, "1.0.54", &draft("2026-11-25", &["修订公告"])).unwrap();
        let (_, entries) = parse_entries(&updated).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].list, ["修订公告"]);
        assert_eq!(entries[0].extra["tag"], "stable");
    }

    #[test]
    fn rejects_archived_versions_invalid_dates_and_empty_notes() {
        let old = r#"[{"version":"1.0.53","date":"2026-11-20","list":["旧公告"]},{"version":"1.0.54","date":"2026-11-24","list":["最新公告"]}]"#;
        assert!(
            write_announcement(old, "1.0.53", &draft("2026-11-25", &["回退"]))
                .unwrap_err()
                .contains("历史公告")
        );
        assert!(write_announcement("[]", "1.0.55", &draft("2026-02-30", &["错误日期"])).is_err());
        assert!(write_announcement("[]", "1.0.55", &draft("2026-11-25", &[" "])).is_err());
        let updated =
            write_announcement("[]", "1.0.55", &draft("2026-11-25", &["首条公告"])).unwrap();
        assert_eq!(parse_entries(&updated).unwrap().1.len(), 1);
    }

    #[test]
    fn version_log_path_rejects_escape_and_package_json() {
        for path in [
            "../versionLog.json",
            "C:/versionLog.json",
            "/versionLog.json",
            "Package.JSON",
            "src/../versionLog.json",
            "data.txt",
        ] {
            assert!(normalize_version_log_path(path).is_err(), "{path}");
        }
        assert_eq!(
            normalize_version_log_path(r"src\config\versionLog.json").unwrap(),
            DEFAULT_VERSION_LOG
        );
    }
}
