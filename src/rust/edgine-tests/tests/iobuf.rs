// SPDX-License-Identifier: MPL-2.0
//! iobuf 容量计算与页对齐单元测试
//!
//! 模拟 framework/iobuf::IobRegion::alloc 的前置逻辑 (总容量 + 页数).
//! host 侧无法调 pmm_alloc_pages, 这里只测纯函数.

use edgine_tests::{iobuf_pages, iobuf_total_capacity};

#[test]
fn test_iobuf_capacity_simple() {
    assert_eq!(iobuf_total_capacity(&[100, 200, 300]), 600);
    assert_eq!(iobuf_total_capacity(&[0, 100, 0, 200]), 300);
    assert_eq!(iobuf_total_capacity(&[]), 0);
    assert_eq!(iobuf_total_capacity(&[0, 0, 0]), 0);
}

#[test]
fn test_iobuf_capacity_overflow() {
    // 模拟两个 iov 总和溢出
    let lens = vec![usize::MAX, 1];
    assert_eq!(iobuf_total_capacity(&lens), 0);
}

#[test]
fn test_iobuf_pages_alignment() {
    assert_eq!(iobuf_pages(0), 1);
    assert_eq!(iobuf_pages(1), 1);
    assert_eq!(iobuf_pages(4096), 1);
    assert_eq!(iobuf_pages(4097), 2);
    assert_eq!(iobuf_pages(8192), 2);
    assert_eq!(iobuf_pages(8193), 3);
    // 突破 4KB 栈缓冲限制 — 旧实现会返 EINVAL, 新实现正常 alloc
    assert_eq!(iobuf_pages(64 * 1024), 16);
}

#[test]
fn test_iobuf_capacity_large_sg() {
    // 模拟 8 个 8KB iov = 64KB SG 拼接, 旧 4KB 栈限制不可行
    let lens = vec![8192usize; 8];
    assert_eq!(iobuf_total_capacity(&lens), 65536);
    assert_eq!(iobuf_pages(65536), 16);
}
