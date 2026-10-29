//! 领域模型：与平台无关，可在任意平台跑单元测试。
//!
//! 本 crate **不允许** 依赖 `windows`，Windows 相关逻辑全部放在 `wp-platform`。

pub mod model;
pub mod package;
pub mod property;

pub use model::{
    Assignment, LibraryItem, MonitorKey, PhysicalRect, WallpaperId, WallpaperKind, WallpaperSpec,
};
pub use package::{Manifest, PackageError};
pub use property::{PropertySchema, PropertyType, PropertyValue, PropertyValues};
