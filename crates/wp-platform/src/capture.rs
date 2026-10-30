//! 屏幕截取。
//!
//! 只服务于 M0 的自动可见性验证：截一张图落盘，由人眼或像素统计判断
//! 「壁纸是否真的画出来了 / 桌面图标是否还在最上层」。
//! 正式功能不依赖它。

use std::io::Write;
use std::mem::size_of;
use std::path::Path;

use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC, GetDIBits,
    ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, CAPTUREBLT, DIB_RGB_COLORS,
    HGDIOBJ, SRCCOPY,
};

use wp_core::PhysicalRect;

use crate::desktop::PlatformError;

/// 截图结果，像素为 24 位 BGR（每行按 4 字节对齐），自下而上。
#[derive(Debug, Clone)]
pub struct ScreenImage {
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub bgr: Vec<u8>,
}

impl ScreenImage {
    /// 取 (x, y) 处的 RGB，原点在左上角。
    pub fn pixel(&self, x: u32, y: u32) -> Option<[u8; 3]> {
        if x >= self.width || y >= self.height {
            return None;
        }
        // 缓冲区是 bottom-up，需要翻转 y。
        let row = self.height - 1 - y;
        let offset = (row * self.stride + x * 3) as usize;
        let b = *self.bgr.get(offset)?;
        let g = *self.bgr.get(offset + 1)?;
        let r = *self.bgr.get(offset + 2)?;
        Some([r, g, b])
    }

    /// 与目标色完全相同的像素占比（0.0–1.0）。
    pub fn ratio_of(&self, color: [u8; 3]) -> f32 {
        let total = (self.width * self.height) as f32;
        if total == 0.0 {
            return 0.0;
        }
        let mut hits = 0u64;
        for y in 0..self.height {
            for x in 0..self.width {
                if self.pixel(x, y) == Some(color) {
                    hits += 1;
                }
            }
        }
        hits as f32 / total
    }

    /// 指定矩形里有多少像素不是目标色（用于判断图标是否盖在壁纸上）。
    pub fn non_matching(&self, rect: PhysicalRect, color: [u8; 3]) -> u64 {
        let mut count = 0u64;
        for y in rect.y.max(0) as u32..(rect.y + rect.height as i32).max(0) as u32 {
            for x in rect.x.max(0) as u32..(rect.x + rect.width as i32).max(0) as u32 {
                if self.pixel(x, y) != Some(color) {
                    count += 1;
                }
            }
        }
        count
    }
}

/// 抓取虚拟桌面的一部分。
pub fn capture(rect: PhysicalRect) -> Result<ScreenImage, PlatformError> {
    let width = rect.width.max(1) as i32;
    let height = rect.height.max(1) as i32;

    unsafe {
        let screen_dc = GetDC(None);
        if screen_dc.0.is_null() {
            return Err(PlatformError::Win32 {
                context: "GetDC(screen)",
                code: 0,
            });
        }
        let mem_dc = CreateCompatibleDC(Some(screen_dc));
        if mem_dc.0.is_null() {
            ReleaseDC(None, screen_dc);
            return Err(PlatformError::Win32 {
                context: "CreateCompatibleDC",
                code: 0,
            });
        }
        let bitmap = CreateCompatibleBitmap(screen_dc, width, height);
        if bitmap.0.is_null() {
            let _ = DeleteDC(mem_dc);
            ReleaseDC(None, screen_dc);
            return Err(PlatformError::Win32 {
                context: "CreateCompatibleBitmap",
                code: 0,
            });
        }

        let previous = SelectObject(mem_dc, HGDIOBJ(bitmap.0));
        let rop = windows::Win32::Graphics::Gdi::ROP_CODE(SRCCOPY.0 | CAPTUREBLT.0);
        let copied = BitBlt(
            mem_dc, 0, 0, width, height, Some(screen_dc), rect.x, rect.y, rop,
        );

        let stride = (width as u32 * 3).div_ceil(4) * 4;
        let mut info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: height,
                biPlanes: 1,
                biBitCount: 24,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut buffer = vec![0u8; (stride * height as u32) as usize];
        let lines = GetDIBits(
            mem_dc,
            bitmap,
            0,
            height as u32,
            Some(buffer.as_mut_ptr() as *mut _),
            &mut info,
            DIB_RGB_COLORS,
        );

        if !previous.0.is_null() {
            SelectObject(mem_dc, previous);
        }
        let _ = DeleteObject(HGDIOBJ(bitmap.0));
        let _ = DeleteDC(mem_dc);
        ReleaseDC(None, screen_dc);

        copied.map_err(|e| PlatformError::Win32 {
            context: "BitBlt",
            code: e.code().0 as u32,
        })?;
        if lines <= 0 {
            return Err(PlatformError::Win32 {
                context: "GetDIBits",
                code: 0,
            });
        }

        Ok(ScreenImage {
            width: width as u32,
            height: height as u32,
            stride,
            bgr: buffer,
        })
    }
}

/// 写下 24 位 BMP（无压缩）。Windows 画图、PowerShell、浏览器都能直接打开。
pub fn save_bmp(path: &Path, image: &ScreenImage) -> std::io::Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }

    let file_size = 14 + 40 + image.bgr.len() as u32;
    let mut file = std::fs::File::create(path)?;

    file.write_all(b"BM")?;
    file.write_all(&file_size.to_le_bytes())?;
    file.write_all(&0u32.to_le_bytes())?;
    file.write_all(&54u32.to_le_bytes())?;

    file.write_all(&40u32.to_le_bytes())?;
    file.write_all(&(image.width as i32).to_le_bytes())?;
    file.write_all(&(image.height as i32).to_le_bytes())?;
    file.write_all(&1u16.to_le_bytes())?;
    file.write_all(&24u16.to_le_bytes())?;
    file.write_all(&0u32.to_le_bytes())?;
    file.write_all(&(image.bgr.len() as u32).to_le_bytes())?;
    file.write_all(&2835i32.to_le_bytes())?;
    file.write_all(&2835i32.to_le_bytes())?;
    file.write_all(&0u32.to_le_bytes())?;
    file.write_all(&0u32.to_le_bytes())?;

    file.write_all(&image.bgr)?;
    file.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_primary_monitor() {
        let image = capture(PhysicalRect::new(0, 0, 64, 64)).expect("截屏失败");
        assert_eq!(image.width, 64);
        assert_eq!(image.height, 64);
        assert_eq!(image.bgr.len(), (image.stride * 64) as usize);
    }

    #[test]
    fn bmp_roundtrip_header() {
        let image = ScreenImage {
            width: 2,
            height: 2,
            stride: 8,
            bgr: vec![0u8; 16],
        };
        let path = std::env::temp_dir().join("wp-capture-test.bmp");
        save_bmp(&path, &image).expect("写 BMP 失败");
        let bytes = std::fs::read(&path).expect("读 BMP 失败");
        assert_eq!(&bytes[0..2], b"BM");
        assert_eq!(bytes.len(), 54 + 16);
        let _ = std::fs::remove_file(&path);
    }
}
