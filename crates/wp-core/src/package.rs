use std::fmt;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::model::WallpaperKind;

/// 壁纸包清单（`.wpkg` 内的 `manifest.json`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub schema: u32,
    pub id: String,
    #[serde(rename = "type")]
    pub kind: WallpaperKind,
    pub title: String,
    /// 相对包根的入口路径，例如 `content/index.html`。
    pub entry: PathBuf,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub min_engine: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PackageError {
    UnknownKind(String),
    InvalidSchema(u32),
    EmptyField(&'static str),
    /// 入口路径含 `..`、绝对路径或盘符。
    PathTraversal(PathBuf),
    Io(String),
    Json(String),
}

impl fmt::Display for PackageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownKind(k) => write!(f, "未知的壁纸类型: {k}"),
            Self::InvalidSchema(v) => write!(f, "不支持的 manifest schema: {v}"),
            Self::EmptyField(k) => write!(f, "manifest 字段为空: {k}"),
            Self::PathTraversal(p) => write!(f, "非法的入口路径: {}", p.display()),
            Self::Io(e) => write!(f, "读取包失败: {e}"),
            Self::Json(e) => write!(f, "manifest 解析失败: {e}"),
        }
    }
}

impl std::error::Error for PackageError {}

pub const MANIFEST_SCHEMA: u32 = 1;

impl Manifest {
    pub fn parse(json: &str) -> Result<Self, PackageError> {
        let m: Manifest = serde_json::from_str(json).map_err(|e| PackageError::Json(e.to_string()))?;
        m.validate()?;
        Ok(m)
    }

    pub fn validate(&self) -> Result<(), PackageError> {
        if self.schema != MANIFEST_SCHEMA {
            return Err(PackageError::InvalidSchema(self.schema));
        }
        if self.id.trim().is_empty() {
            return Err(PackageError::EmptyField("id"));
        }
        if self.title.trim().is_empty() {
            return Err(PackageError::EmptyField("title"));
        }
        validate_relative_path(&self.entry)?;
        Ok(())
    }

    /// 把入口路径解析为包内绝对路径，并确保没有逃出包根。
    pub fn resolve_entry(&self, package_root: &Path) -> Result<PathBuf, PackageError> {
        validate_relative_path(&self.entry)?;
        Ok(package_root.join(&self.entry))
    }
}

/// 只允许普通相对路径组件：拒绝绝对路径、盘符、`..`、根目录。
pub fn validate_relative_path(path: &Path) -> Result<(), PackageError> {
    if path.as_os_str().is_empty() {
        return Err(PackageError::PathTraversal(path.to_path_buf()));
    }
    for comp in path.components() {
        match comp {
            Component::Normal(_) => {}
            Component::CurDir => {}
            _ => return Err(PackageError::PathTraversal(path.to_path_buf())),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const OK: &str = r#"{
        "schema": 1,
        "id": "abc",
        "type": "web",
        "title": "Neon Rain",
        "entry": "content/index.html"
    }"#;

    #[test]
    fn parses_valid_manifest() {
        let m = Manifest::parse(OK).unwrap();
        assert_eq!(m.kind, WallpaperKind::Web);
        assert_eq!(m.entry, PathBuf::from("content/index.html"));
    }

    #[test]
    fn rejects_unknown_kind() {
        let bad = OK.replace("\"web\"", "\"gif\"");
        assert!(matches!(
            Manifest::parse(&bad),
            Err(PackageError::Json(_)) | Err(PackageError::UnknownKind(_))
        ));
    }

    #[test]
    fn rejects_traversal() {
        // 注意：这是 JSON 文本，反斜杠必须成对转义（Rust 层是 `\\\\`）。
        for evil in ["../secret.txt", "C:\\\\Windows\\\\win.ini", "/etc/passwd"] {
            let bad = OK.replace("content/index.html", evil);
            let err = Manifest::parse(&bad).unwrap_err();
            assert!(
                matches!(err, PackageError::PathTraversal(_)),
                "{evil} 应被拒绝，实际: {err:?}"
            );
        }
    }

    #[test]
    fn rejects_traversal_without_json_layer() {
        // 直接测路径校验，排除 JSON 转义带来的假通过。
        for evil in [
            PathBuf::from("../secret.txt"),
            PathBuf::from("C:\\Windows\\win.ini"),
            PathBuf::from("/etc/passwd"),
            PathBuf::from("content/../../escape"),
        ] {
            assert!(
                matches!(validate_relative_path(&evil), Err(PackageError::PathTraversal(_))),
                "{} 应被拒绝",
                evil.display()
            );
        }
        assert!(validate_relative_path(Path::new("content/index.html")).is_ok());
    }

    #[test]
    fn rejects_bad_schema() {
        let bad = OK.replace("\"schema\": 1", "\"schema\": 99");
        assert_eq!(Manifest::parse(&bad).unwrap_err(), PackageError::InvalidSchema(99));
    }

    #[test]
    fn resolve_stays_inside_root() {
        let m = Manifest::parse(OK).unwrap();
        let p = m.resolve_entry(Path::new("C:\\lib\\abc")).unwrap();
        assert!(p.starts_with("C:\\lib\\abc"));
    }
}
