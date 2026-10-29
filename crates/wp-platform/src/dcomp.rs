//! Direct3D11 + DirectComposition 表面。
//!
//! M0 用来验证最关键的一格：**Progman 子窗口 + DComp visual 的内容能不能进 DWM 合成**。
//! 只有这一格成立，「画面可见 + 图标在上 + 输入穿透」才可能同时满足。
//!
//! 注意：窗口带 `WS_EX_NOREDIRECTIONBITMAP` 时，GDI 画不进去，必须走 DComp；
//! 这是设计使然，不是缺陷。

use windows::core::Interface;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_10_1, D3D_FEATURE_LEVEL_11_0,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11RenderTargetView, ID3D11Texture2D,
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION,
};
use windows::Win32::Graphics::DirectComposition::{
    DCompositionCreateDevice, IDCompositionDevice, IDCompositionTarget, IDCompositionVisual,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE_IGNORE, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    IDXGIDevice, IDXGIFactory2, IDXGISwapChain1, DXGI_PRESENT, DXGI_SCALING_STRETCH,
    DXGI_SWAP_CHAIN_DESC1, DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL, DXGI_USAGE_RENDER_TARGET_OUTPUT,
};

use crate::desktop::PlatformError;
use crate::window::raw;

/// 一块挂在窗口上的 DComp 表面。
pub struct DcompSurface {
    /// 只需保证 D3D 设备活到表面销毁，不直接读，用下划线避免告警。
    _device: ID3D11Device,
    context: ID3D11DeviceContext,
    swap_chain: IDXGISwapChain1,
    rtv: ID3D11RenderTargetView,
    // 保持存活：visual/target/device 释放后画面立即消失。
    _visual: IDCompositionVisual,
    _target: IDCompositionTarget,
    _dcomp: IDCompositionDevice,
    width: u32,
    height: u32,
}

impl DcompSurface {
    /// `topmost=true` 时 visual 盖在窗口内容/子窗口之上；`false` 时在其下方。
    ///
    /// 策略 E（把 DefView 挂成子窗口）必须用 `false`，否则壁纸会盖住图标。
    pub fn new(
        hwnd: isize,
        width: u32,
        height: u32,
        topmost: bool,
    ) -> Result<Self, PlatformError> {
        unsafe {
            let mut device: Option<ID3D11Device> = None;
            let mut context: Option<ID3D11DeviceContext> = None;
            let mut level = D3D_FEATURE_LEVEL_11_0;

            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                Some(&[D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_10_1]),
                D3D11_SDK_VERSION,
                Some(&mut device),
                Some(&mut level),
                Some(&mut context),
            )
            .map_err(|e| win32("D3D11CreateDevice", e))?;

            let device = device.ok_or(PlatformError::Unsupported("D3D11 设备创建返回空"))?;
            let context = context.ok_or(PlatformError::Unsupported("D3D11 上下文创建返回空"))?;

            let dxgi_device: IDXGIDevice =
                device.cast().map_err(|e| win32("IDXGIDevice", e))?;
            let adapter = dxgi_device
                .GetAdapter()
                .map_err(|e| win32("IDXGIDevice::GetAdapter", e))?;
            let factory: IDXGIFactory2 = adapter.GetParent().map_err(|e| win32("IDXGIFactory2", e))?;

            let desc = DXGI_SWAP_CHAIN_DESC1 {
                Width: width,
                Height: height,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                Stereo: false.into(),
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                BufferCount: 2,
                Scaling: DXGI_SCALING_STRETCH,
                SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
                AlphaMode: DXGI_ALPHA_MODE_IGNORE,
                Flags: 0,
            };
            let swap_chain = factory
                .CreateSwapChainForComposition(&device, &desc, None)
                .map_err(|e| win32("CreateSwapChainForComposition", e))?;

            let dcomp: IDCompositionDevice =
                DCompositionCreateDevice(&dxgi_device).map_err(|e| win32("DCompositionCreateDevice", e))?;
            let target = dcomp
                .CreateTargetForHwnd(raw(hwnd), topmost)
                .map_err(|e| win32("CreateTargetForHwnd", e))?;
            let visual = dcomp
                .CreateVisual()
                .map_err(|e| win32("CreateVisual", e))?;
            visual
                .SetContent(&swap_chain)
                .map_err(|e| win32("SetContent", e))?;
            target
                .SetRoot(&visual)
                .map_err(|e| win32("SetRoot", e))?;
            dcomp.Commit().map_err(|e| win32("Commit", e))?;

            let rtv = create_rtv(&device, &swap_chain)?;

            Ok(Self {
                _device: device,
                context,
                swap_chain,
                rtv,
                _visual: visual,
                _target: target,
                _dcomp: dcomp,
                width,
                height,
            })
        }
    }

    /// 清成纯色并提交一帧。`rgb` 是 0–255 的 RGB。
    pub fn clear(&mut self, rgb: [u8; 3]) -> Result<(), PlatformError> {
        let color = [
            rgb[0] as f32 / 255.0,
            rgb[1] as f32 / 255.0,
            rgb[2] as f32 / 255.0,
            1.0,
        ];
        unsafe {
            self.context.ClearRenderTargetView(&self.rtv, &color);
            self.swap_chain
                .Present(1, DXGI_PRESENT(0))
                .ok()
                .map_err(|e| win32("Present", e))?;
        }
        Ok(())
    }

    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }
}

fn create_rtv(
    device: &ID3D11Device,
    swap_chain: &IDXGISwapChain1,
) -> Result<ID3D11RenderTargetView, PlatformError> {
    unsafe {
        let buffer: ID3D11Texture2D = swap_chain
            .GetBuffer(0)
            .map_err(|e| win32("GetBuffer", e))?;
        let mut rtv: Option<ID3D11RenderTargetView> = None;
        device
            .CreateRenderTargetView(&buffer, None, Some(&mut rtv))
            .map_err(|e| win32("CreateRenderTargetView", e))?;
        rtv.ok_or(PlatformError::Unsupported("渲染目标创建返回空"))
    }
}

fn win32(context: &'static str, e: windows::core::Error) -> PlatformError {
    PlatformError::Win32 {
        context,
        code: e.code().0 as u32,
    }
}
