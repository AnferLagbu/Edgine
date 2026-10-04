//! `UNKFS` ARC 缓存 safe API 封装
//!
//! 提供 `ptr_to_slice` 将裸指针转为切片, 封装 `from_raw_parts` unsafe.
//! services 层通过此 safe API 访问 ARC 缓存数据, 无需自行写 unsafe.

/// 将裸指针和长度转为 `&[u8]` 切片
///
/// # Safety 保证
///
/// 调用方 (framework 层 ARC) 保证指针有效:
/// - 指针来自 ARC 缓存的 `NestArcBuf::data` 字段, 生命周期不超过 ARC 缓存本身
/// - ARC 缓存是全局静态变量, 数据生命周期为 `'static`
/// - `len` 不超过 `NestArcBuf::data` 实际长度
pub fn ptr_to_slice<'a>(ptr: *const u8, len: usize) -> Option<&'a [u8]> {
    if ptr.is_null() || len == 0 {
        return None;
    }
    // SAFETY: 调用方保证指针来自 ARC 缓存的 NestArcBuf::data,
    // 该数据存储在全局静态 ARC 中, 生命周期为 'static.
    // len 由调用方保证不超过 data 实际长度.
    Some(unsafe { core::slice::from_raw_parts(ptr, len) })
}
