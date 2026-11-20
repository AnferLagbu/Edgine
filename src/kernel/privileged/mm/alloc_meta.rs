//! 全局内核分配器 (`kernel::memory_allocator`) 的尾随元数据布局算式
//!
//! 全局分配器把「来源标签」与「对齐偏移」两类元数据写在**载荷之后**, 即
//! `[数据指针 + size, 数据指针 + size + META_LEN)`; 数据指针本身由基础块指针按
//! `layout.align()` 上抬得到。因此:
//!
//! - 数据指针就是基础块指针 (页对齐分配时偏移为 0), 不再被头部标签破坏对齐;
//! - dealloc 只需同一 `Layout` 即可定位元数据, 并从 `size`/偏移复原基础块指针。
//!
//! 本模块只放**纯算式** (不触碰指针、不依赖 arch), 故可在 host 侧内联单测;
//! 真正的分配/归还流程仍只在 `memory_allocator` 中 (该文件被 host-test feature 排除)。

/// 尾随元数据长度: 来源标签 `u64` + 对齐偏移 `u64`。
pub const META_LEN: usize = 16;

/// 页粒度 (PMM 单页字节数), 也是 PMM 路径基础块指针的对齐保证。
pub const PAGE_BYTES: usize = 4096;

/// 把页对齐的基础块指针上抬到 `align` 所需的最小字节偏移。
///
/// `base` 须为 `align` 的合法基址 (PMM 路径页对齐、kmalloc 路径至少 8 字节对齐);
/// 返回值恒 `< align`, 且 `base + 返回值` 是 `align` 的整数倍。
/// `align` 由 `Layout` 保证为非零 2 的幂。
#[must_use]
pub fn align_offset(base: usize, align: usize) -> usize {
    let rem = base % align;
    if rem == 0 { 0 } else { align - rem }
}

/// 覆盖「载荷 + 尾随元数据 + 最坏对齐偏移」所需的页数 (PMM 路径)。
///
/// `base` 页对齐时, `align <= PAGE_BYTES` 的偏移恒为 0 (页对齐已是其整数倍),
/// 故最坏偏移只在大对齐时出现: `align > PAGE_BYTES` 时下一个对齐边界距页基址
/// 至多 `align - PAGE_BYTES`。返回 `None` 表示字节数溢出 `usize` (调用方按分配
/// 失败处理)。
#[must_use]
pub fn pages_for(size: usize, align: usize) -> Option<usize> {
    let worst_offset = align.saturating_sub(PAGE_BYTES);
    let total = size.checked_add(META_LEN)?.checked_add(worst_offset)?;
    // 向上取整到整页; 单独 checked 一次, 避免 total + PAGE_BYTES - 1 回绕。
    let rounded = total.checked_add(PAGE_BYTES - 1)?;
    Some(rounded / PAGE_BYTES)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meta_len_is_two_words() {
        assert_eq!(META_LEN, 2 * core::mem::size_of::<u64>());
    }

    #[test]
    fn page_aligned_base_needs_no_offset_up_to_page_align() {
        // PMM 路径: 基址页对齐, 任何 <= 页的对齐都无需偏移 (否则整页分配会被白占)。
        let base = 0x2000_0000;
        for shift in 0..=12u32 {
            let align = 1usize << shift;
            assert_eq!(
                align_offset(base, align),
                0,
                "页对齐基址对 align={align} 不应有偏移"
            );
        }
    }

    #[test]
    fn align_offset_rounds_up_within_one_step() {
        // kmalloc 路径: 基址至少 8 字节对齐, 但未必满足更大对齐 ⇒ 偏移须封顶在 align 内。
        for align in [
            8usize, 16, 32, 64, 128, 256, 512, 1024, 2048, 4096, 8192, 16384,
        ] {
            for k in 0..5usize {
                let base = 0x1000 * (k + 1) + 8;
                let offset = align_offset(base, align);
                assert!(offset < align, "偏移必须小于一步对齐");
                assert_eq!((base + offset) % align, 0, "上抬后必须对齐");
            }
        }
    }

    #[test]
    fn pages_for_covers_payload_metadata_and_worst_offset() {
        for align in [1usize, 2, 4, 8, 16, 32, 4096, 8192, 16384] {
            let worst_offset = align.saturating_sub(PAGE_BYTES);
            for size in [
                0usize, 1, 7, 8, 15, 16, 17, 100, 2048, 2049, 4080, 4096, 8191,
            ] {
                let pages = pages_for(size, align).expect("普通尺寸不应溢出");
                let need = size + META_LEN + worst_offset;
                assert!(
                    pages * PAGE_BYTES >= need,
                    "页数必须覆盖载荷+元数据+最坏偏移: size={size} align={align}"
                );
                assert!(
                    (pages - 1) * PAGE_BYTES < need,
                    "页数必须是最小值: size={size} align={align}"
                );
            }
        }
    }

    #[test]
    fn pages_for_rejects_overflow() {
        assert_eq!(pages_for(usize::MAX, 1), None);
        assert_eq!(pages_for(usize::MAX - META_LEN, 4096), None);
    }
}
