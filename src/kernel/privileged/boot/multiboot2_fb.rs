//! Multiboot2 帧缓冲信息解析
//!
//! 解析 Multiboot2 `FRAMEBUFFER_INFO` tag (type=8)，
//! 提取物理帧缓冲地址、分辨率、像素格式等信息。
//!
//! # Safety
//! `FB_INFO` 使用 `spin::Once` 确保启动早期写入一次，之后只读。
//! `FRAMEBUFFER_TAG_TYPE` = 8 对应 Multiboot2 spec 3.6.12。

use crate::privileged::sync::OnceLock;
pub const MULTIBOOT2_TAG_FRAMEBUFFER: u32 = 8;

#[derive(Debug, Clone, Copy)]
pub struct FramebufferInfo {
    pub addr: u64,
    pub pitch: u32,
    pub width: u32,
    pub height: u32,
    pub bpp: u8,
    pub fb_type: u8,
    pub red_field_position: u8,
    pub red_mask_size: u8,
    pub green_field_position: u8,
    pub green_mask_size: u8,
    pub blue_field_position: u8,
    pub blue_mask_size: u8,
}

impl FramebufferInfo {
    pub const fn new() -> Self {
        Self {
            addr: 0,
            pitch: 0,
            width: 0,
            height: 0,
            bpp: 0,
            fb_type: 0,
            red_field_position: 0,
            red_mask_size: 0,
            green_field_position: 0,
            green_mask_size: 0,
            blue_field_position: 0,
            blue_mask_size: 0,
        }
    }

    pub fn is_valid(&self) -> bool {
        self.addr != 0 && self.width > 0 && self.height > 0 && self.bpp > 0
    }
}

static FB_INFO: OnceLock<FramebufferInfo> = OnceLock::new();

pub fn get_framebuffer_info() -> Option<&'static FramebufferInfo> {
    FB_INFO.get()
}

#[expect(
    clippy::ptr_as_ptr,
    reason = "指针类型 cast 不变 constness (e.g. *mut T → *mut U); 改 .cast() 是机械替换不治根, 当前优先 expect 兑底"
)]
#[expect(
    clippy::cast_ptr_alignment,
    reason = "cast_ptr_alignment: 指针类型转换对齐假设已知安全 (例如硬件 MMIO 寄存器地址已知对齐; 当前优先 expect"
)]
/// 从 Multiboot2 tag 中解析帧缓冲信息
///
/// `tag_data` 指向 tag 的 payload 起始位置 (tag 头部的 type/size 之后)
/// `tag_size` 是整个 tag 的大小 (包含 type/size 头部)
///
/// tag type=8 的 layout (Multiboot2 spec 3.6.12):
///   u32  type = 8
///   u32  size
///   u64  `framebuffer_addr`
///   u32  `framebuffer_pitch`
///   u32  `framebuffer_width`
///   u32  `framebuffer_height`
///   u8   `framebuffer_bpp`
///   u8   `framebuffer_type`    (0=索引色, 1=RGB, 2=文本)
///   u16  reserved
///   — 若 type==1 (RGB):
///   u8   `red_field_position`
///   u8   `red_mask_size`
///   u8   `green_field_position`
///   u8   `green_mask_size`
///   u8   `blue_field_position`
///   u8   `blue_mask_size`
pub fn parse_framebuffer_tag(tag_data: *const u8, _tag_size: u32) {
    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    unsafe {
        let addr = *(tag_data as *const u64);
        let pitch = *((tag_data as *const u32).add(2));
        let width = *((tag_data as *const u32).add(3));
        let height = *((tag_data as *const u32).add(4));
        let bpp = *(tag_data.add(20));
        let fb_type = *(tag_data.add(21));

        let mut fb = FramebufferInfo {
            addr,
            pitch,
            width,
            height,
            bpp,
            fb_type,
            ..FramebufferInfo::new()
        };

        if fb_type == 1 {
            fb.red_field_position = *(tag_data.add(24));
            fb.red_mask_size = *(tag_data.add(25));
            fb.green_field_position = *(tag_data.add(26));
            fb.green_mask_size = *(tag_data.add(27));
            fb.blue_field_position = *(tag_data.add(28));
            fb.blue_mask_size = *(tag_data.add(29));
        }

        FB_INFO.get_or_init(|slot| {
            slot.write(fb);
        });
    }
}
