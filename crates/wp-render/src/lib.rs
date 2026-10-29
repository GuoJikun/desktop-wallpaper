//! 渲染后端抽象。
//!
//! 刻意拆成两类：
//! - [`Renderer`]：由我们每帧驱动（图片、幻灯片、wgpu 场景）
//! - [`Presenter`]：由外部组件自渲染，我们只控制（mpv、WebView2）

use wp_core::WallpaperSpec;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhysicalSize {
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct FrameCtx {
    /// 自启动以来的秒数。
    pub time: f32,
    /// 距上一帧的秒数。
    pub delta: f32,
    /// 光标位置（壁纸本地坐标）。
    pub cursor: (i32, i32),
    /// 光标是否在壁纸区域内。
    pub cursor_inside: bool,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Stats {
    pub fps: f32,
    pub cpu_percent: f32,
    pub gpu_mem_mb: u32,
    pub dropped_frames: u64,
}

#[derive(Debug)]
pub enum RenderError {
    Init(String),
    Decode(String),
    Unsupported(String),
    Io(String),
}

impl std::fmt::Display for RenderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Init(e) => write!(f, "后端初始化失败: {e}"),
            Self::Decode(e) => write!(f, "解码失败: {e}"),
            Self::Unsupported(e) => write!(f, "不支持: {e}"),
            Self::Io(e) => write!(f, "IO 错误: {e}"),
        }
    }
}

impl std::error::Error for RenderError {}

pub type Result<T> = std::result::Result<T, RenderError>;

/// 由我们驱动的渲染后端。
pub trait Renderer: Send {
    fn resize(&mut self, size: PhysicalSize) -> Result<()>;
    fn render(&mut self, ctx: &FrameCtx) -> Result<()>;
    fn set_visible(&mut self, visible: bool);
    fn set_target_fps(&mut self, fps: u32);
    fn stats(&self) -> Stats;
}

/// 外部组件自渲染（mpv / WebView2）。
pub trait Presenter: Send {
    fn load(&mut self, spec: &WallpaperSpec) -> Result<()>;
    fn resize(&mut self, size: PhysicalSize) -> Result<()>;
    fn play(&mut self) -> Result<()>;
    fn pause(&mut self) -> Result<()>;
    fn set_muted(&mut self, muted: bool) -> Result<()>;
    fn set_visible(&mut self, visible: bool);
    fn stats(&self) -> Stats;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    Image,
    Video,
    Web,
    Scene,
}

impl BackendKind {
    pub fn from_wallpaper(kind: wp_core::WallpaperKind) -> Self {
        use wp_core::WallpaperKind as K;
        match kind {
            K::Image => Self::Image,
            K::Video => Self::Video,
            K::Web => Self::Web,
            K::Scene => Self::Scene,
            // 任意外部程序按 presenter 处理，MVP 阶段先拒绝。
            K::Application => Self::Web,
        }
    }
}
