//! 图片壁纸后端：解码 → cover 缩放 → BGRA8 像素缓冲。
//!
//! 缩放刻意放在 CPU 上做：静态图片只需要在加载时缩一次，之后每帧都是同一块内存，
//! GPU 侧只要一次 `UpdateSubresource`，不需要着色器、顶点缓冲和采样器状态。
//! 代价是加载时多花几十毫秒（4K → 1080p 约 100 ms），换来的是整条管线没有 D3D
//! 状态机依赖，`wp-platform` 之外也能单测。

use std::path::Path;

use crate::{PhysicalSize, RenderError, Result};

/// 一张已经准备好上传的 BGRA8 图片。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BgraImage {
    pub width: u32,
    pub height: u32,
    /// 长度恒等于 `width * height * 4`，字节顺序是 B、G、R、A。
    pub pixels: Vec<u8>,
}

impl BgraImage {
    /// 每行字节数。交换链缓冲是紧密排列的，所以等于 `width * 4`。
    pub fn stride(&self) -> u32 {
        self.width * 4
    }

    pub fn is_empty(&self) -> bool {
        self.pixels.is_empty()
    }

    /// 取某个像素的 RGBA（按人的直觉顺序），只用于测试与诊断。
    pub fn rgba_at(&self, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * self.width + x) * 4) as usize;
        [
            self.pixels[i + 2],
            self.pixels[i + 1],
            self.pixels[i],
            self.pixels[i + 3],
        ]
    }
}

/// 读取图片文件并缩放到「填满目标区域」的尺寸。
pub fn decode_file(path: &Path, target: PhysicalSize) -> Result<BgraImage> {
    let bytes = std::fs::read(path)
        .map_err(|e| RenderError::Io(format!("读取 {} 失败: {e}", path.display())))?;
    decode(&bytes, target).map_err(|e| match e {
        RenderError::Decode(msg) => RenderError::Decode(format!("{}: {msg}", path.display())),
        other => other,
    })
}

/// 解码内存里的图片并缩放到「填满目标区域」的尺寸。
///
/// 支持 PNG / JPEG / BMP（只开了这三个解码器，减少依赖体积）。
pub fn decode(bytes: &[u8], target: PhysicalSize) -> Result<BgraImage> {
    let (tw, th) = (target.width, target.height);
    if tw == 0 || th == 0 {
        return Err(RenderError::Unsupported(format!("目标尺寸非法: {tw}x{th}")));
    }

    let image = image::load_from_memory(bytes)
        .map_err(|e| RenderError::Decode(format!("无法识别的图片格式: {e}")))?;
    let (sw, sh) = (image.width(), image.height());
    if sw == 0 || sh == 0 {
        return Err(RenderError::Decode("图片尺寸为 0".into()));
    }

    let (cw, ch) = cover_size((sw, sh), (tw, th));
    let scaled = if (cw, ch) == (sw, sh) {
        image
    } else {
        // Triangle 比 Lanczos3 快一个数量级，1080p 缩放后的画质差异肉眼不可见，
        // 而壁纸加载是用户能感知到的等待。
        image.resize_exact(cw, ch, image::imageops::FilterType::Triangle)
    };

    // cover：居中裁掉超出目标比例的部分。
    let cropped = if (cw, ch) == (tw, th) {
        scaled
    } else {
        let x = (cw - tw) / 2;
        let y = (ch - th) / 2;
        scaled.crop_imm(x, y, tw, th)
    };

    Ok(to_bgra(cropped))
}

/// cover 缩放后的中间尺寸：等比放大到刚好填满目标，多出来的部分由调用方居中裁掉。
///
/// 用整数交叉相乘比较宽高比，避免 `f32` 在 4K 尺寸上的舍入误差。
pub fn cover_size(src: (u32, u32), target: (u32, u32)) -> (u32, u32) {
    let (sw, sh) = (src.0 as u64, src.1 as u64);
    let (tw, th) = (target.0 as u64, target.1 as u64);
    if sw == 0 || sh == 0 || tw == 0 || th == 0 {
        return (target.0.max(1), target.1.max(1));
    }
    if tw * sh >= th * sw {
        // 源图更「高」：先让宽度填满，高度会超出目标。
        let h = (sh * tw).div_ceil(sw);
        (tw as u32, h.max(th) as u32)
    } else {
        // 源图更「宽」：先让高度填满，宽度会超出目标。
        let w = (sw * th).div_ceil(sh);
        (w.max(tw) as u32, th as u32)
    }
}

/// 转成交换链要的 BGRA 顺序。`image` 统一先转 RGBA8，再交换 R/B 两个通道。
fn to_bgra(image: image::DynamicImage) -> BgraImage {
    let mut rgba = image.into_rgba8();
    for pixel in rgba.pixels_mut() {
        pixel.0.swap(0, 2);
    }
    BgraImage {
        width: rgba.width(),
        height: rgba.height(),
        pixels: rgba.into_raw(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(width: u32, height: u32, rgba: [u8; 4]) -> Vec<u8> {
        let mut buf = Vec::new();
        let img = image::RgbaImage::from_pixel(width, height, image::Rgba(rgba));
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
            .expect("编码测试 PNG");
        buf
    }

    #[test]
    fn cover_size_matches_aspect() {
        assert_eq!(cover_size((1920, 1080), (1920, 1080)), (1920, 1080));
        assert_eq!(cover_size((1920, 1080), (3840, 2160)), (3840, 2160));
    }

    #[test]
    fn cover_size_fills_and_overflows() {
        // 竖图：宽度撑满，高度超出。
        assert_eq!(cover_size((1080, 1920), (1920, 1080)), (1920, 3414));
        // 超宽图：高度撑满，宽度超出。
        assert_eq!(cover_size((4000, 1000), (1920, 1080)), (4320, 1080));
    }

    #[test]
    fn cover_size_survives_zero() {
        assert_eq!(cover_size((0, 100), (1920, 1080)), (1920, 1080));
        assert_eq!(cover_size((100, 100), (0, 0)), (1, 1));
    }

    #[test]
    fn decode_scales_to_target_and_swaps_channels() {
        // 红色必须落到 BGRA 的最后一个通道前面：B=0x00 G=0x00 R=0xFF A=0xFF。
        let bytes = png(2, 1, [0xFF, 0x00, 0x00, 0xFF]);
        let image = decode(
            &bytes,
            PhysicalSize {
                width: 4,
                height: 4,
            },
        )
        .unwrap();
        assert_eq!((image.width, image.height), (4, 4));
        assert_eq!(image.pixels.len(), 4 * 4 * 4);
        assert_eq!(image.rgba_at(0, 0), [0xFF, 0x00, 0x00, 0xFF]);
        assert_eq!(image.stride(), 16);
    }

    #[test]
    fn decode_crops_center_of_wide_source() {
        // 左半边绿、右半边蓝：cover 到正方形时应保留中间的混合区而不是整条边。
        let mut img = image::RgbaImage::new(4, 2);
        for (x, _, p) in img.enumerate_pixels_mut() {
            *p = if x < 2 {
                image::Rgba([0, 0xFF, 0, 0xFF])
            } else {
                image::Rgba([0, 0, 0xFF, 0xFF])
            };
        }
        let mut buf = Vec::new();
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
            .unwrap();
        // 源 4x2（2:1）→ 目标 2x2（1:1）：先放大到 4x4，再居中裁出中间 2 列。
        let out = decode(
            &buf,
            PhysicalSize {
                width: 2,
                height: 2,
            },
        )
        .unwrap();
        assert_eq!((out.width, out.height), (2, 2));
        assert_eq!(out.rgba_at(0, 0), [0, 0xFF, 0, 0xFF]);
        assert_eq!(out.rgba_at(1, 0), [0, 0, 0xFF, 0xFF]);
    }

    #[test]
    fn decode_rejects_garbage() {
        let err = decode(
            b"not an image",
            PhysicalSize {
                width: 16,
                height: 16,
            },
        )
        .unwrap_err();
        assert!(matches!(err, RenderError::Decode(_)));
    }

    #[test]
    fn decode_rejects_zero_target() {
        let bytes = png(1, 1, [0, 0, 0, 0xFF]);
        let err = decode(
            &bytes,
            PhysicalSize {
                width: 0,
                height: 1080,
            },
        )
        .unwrap_err();
        assert!(matches!(err, RenderError::Unsupported(_)));
    }
}
