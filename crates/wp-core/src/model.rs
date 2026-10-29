use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// 壁纸唯一标识。使用 UUID 而非自增 ID，便于导入/导出后保持一致。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct WallpaperId(pub Uuid);

impl WallpaperId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for WallpaperId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for WallpaperId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WallpaperKind {
    Image,
    Video,
    Web,
    Scene,
    Application,
}

impl WallpaperKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Image => "image",
            Self::Video => "video",
            Self::Web => "web",
            Self::Scene => "scene",
            Self::Application => "application",
        }
    }
}

impl std::str::FromStr for WallpaperKind {
    type Err = crate::package::PackageError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "image" => Ok(Self::Image),
            "video" => Ok(Self::Video),
            "web" => Ok(Self::Web),
            "scene" => Ok(Self::Scene),
            "application" => Ok(Self::Application),
            other => Err(crate::package::PackageError::UnknownKind(other.to_string())),
        }
    }
}

/// 显示器键：用设备名（如 `\\.\DISPLAY1`）而不是索引，避免热插拔后错位。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MonitorKey(pub String);

impl MonitorKey {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for MonitorKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// 物理像素矩形（左上角 + 尺寸），坐标空间为虚拟桌面。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhysicalRect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl PhysicalRect {
    pub fn new(x: i32, y: i32, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LibraryItem {
    pub id: WallpaperId,
    pub kind: WallpaperKind,
    pub title: String,
    /// 包目录或单个媒体文件。
    pub source: PathBuf,
    pub preview: Option<PathBuf>,
    pub tags: Vec<String>,
    pub properties: crate::PropertySchema,
    pub added_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Assignment {
    pub monitor: MonitorKey,
    pub item: WallpaperId,
    pub overrides: crate::PropertyValues,
}

/// 下发给渲染进程的完整描述（自包含，避免 host 反查数据库）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WallpaperSpec {
    pub id: WallpaperId,
    pub kind: WallpaperKind,
    pub entry: PathBuf,
    pub rect: PhysicalRect,
    pub properties: crate::PropertyValues,
    pub muted: bool,
    pub paused: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_is_unique_and_roundtrips() {
        let a = WallpaperId::new();
        let b = WallpaperId::new();
        assert_ne!(a, b);

        let json = serde_json::to_string(&a).unwrap();
        let back: WallpaperId = serde_json::from_str(&json).unwrap();
        assert_eq!(a, back);
    }

    #[test]
    fn kind_str_roundtrip() {
        use std::str::FromStr;
        for k in [
            WallpaperKind::Image,
            WallpaperKind::Video,
            WallpaperKind::Web,
            WallpaperKind::Scene,
        ] {
            assert_eq!(WallpaperKind::from_str(k.as_str()).unwrap(), k);
        }
        assert!(WallpaperKind::from_str("gif").is_err());
    }
}
