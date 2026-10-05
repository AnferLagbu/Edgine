//! 单生产者单消费者环形缓冲区 (SPSC Ring Buffer)
//!
//! ## 设计目标
//!
//! - 无锁: 生产者与消费者分属不同上下文, 各自持有一个原子下标
//! - 容量: 2 的幂, 用掩码代替取模
//! - 支持回绕: 旧事件在缓冲区满时被覆盖
//!
//! ## 不变式
//!
//! - `head` 永远 >= `tail` (单调递增)
//! - `head - tail <= capacity`
//! - 在容量不足时, 旧事件被覆盖; 消费者只读取 `tail..head`
//!
//! ## 内存序
//!
//! 生产者 release 写 head, 消费者 acquire 读 head;
//! 反之消费者 release 写 tail, 生产者 acquire 读 tail。
//!
//! ## SAFETY 不变式
//!
//! - 仅 SPSC: 不支持多生产者, 也不支持多消费者
//! - 容量为 2 的幂, 否则索引/掩码计算错误

use core::sync::atomic::{AtomicUsize, Ordering};

/// 默认 ring buffer 容量 (4 KiB, 256 个 16 字节事件)
pub const DEFAULT_RING_CAPACITY: usize = 4096;

/// SPSC 环形缓冲区
pub struct RingBuffer<const CAP: usize> {
    /// 数据存储
    data: [u8; CAP],
    /// 生产者下标 (字节偏移, 单调)
    head: AtomicUsize,
    /// 消费者下标 (字节偏移, 单调)
    tail: AtomicUsize,
}

impl<const CAP: usize> RingBuffer<{ CAP }> {
    /// 创建空缓冲区
    ///
    /// # Panics
    ///
    /// 若 CAP 不是 2 的幂则在 debug 构建中 panic, release 中依赖掩码
    /// 计算的正确性。
    const fn assert_power_of_two() {
        assert!(CAP > 0 && CAP.is_power_of_two(), "CAP must be power of 2");
    }

    /// 创建空环形缓冲区, 构造期断言容量为 2 的幂
    pub const fn new() -> Self {
        Self::assert_power_of_two();
        Self {
            data: [0u8; CAP],
            head: AtomicUsize::new(0),
            tail: AtomicUsize::new(0),
        }
    }

    /// 容量
    #[inline]
    #[expect(
        clippy::unused_self,
        reason = "保留 &self 签名以便调用点统一用法, 不依赖 self 字段时可改关联函数"
    )]
    pub const fn capacity(&self) -> usize {
        CAP
    }

    /// 当前可用字节数
    #[inline]
    pub fn len(&self) -> usize {
        let h = self.head.load(Ordering::Acquire);
        let t = self.tail.load(Ordering::Acquire);
        h.wrapping_sub(t)
    }

    /// 是否为空
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// 推入一段字节, 容量不足时覆盖最旧数据
    ///
    /// 返回实际写入的字节数 (永远 == `data.len()`)。
    pub fn push(&self, src: &[u8]) -> usize {
        let len = src.len();
        if len == 0 {
            return 0;
        }
        if len >= CAP {
            // 单次写入超过总容量, 截断到 CAP
            let off = CAP - len;
            let slot = &src[len - CAP..];
            self.write_into(slot, off);
            return CAP;
        }

        let h = self.head.load(Ordering::Relaxed);
        // 推进 head 到 h + len
        let new_head = h.wrapping_add(len);
        // 维护回绕: 若 len > CAP, 推进 tail
        if (new_head - self.tail.load(Ordering::Relaxed)) > CAP {
            // 覆盖: 推进 tail 使之满足 head - tail == CAP
            let new_tail = new_head.wrapping_sub(CAP);
            self.tail.store(new_tail, Ordering::Release);
        }
        // 写入数据到 h..h+len
        self.write_into(src, h);
        // release 发布 head
        self.head.store(new_head, Ordering::Release);
        len
    }

    #[expect(
        clippy::ptr_cast_constness,
        reason = "ptr_cast_constness: *mut T as *const T 是已知安全 (Rust 2024 可用 ptr.cast_const 或 &raw const; 当前优先 expect"
    )]
    /// 把 src 写入到以 `abs_off` 起始的环形位置
    fn write_into(&self, src: &[u8], abs_off: usize) {
        let cap_mask = CAP - 1;
        let start = abs_off & cap_mask;
        let len = src.len();
        if start + len <= CAP {
            let dst = &self.data[start..start + len];
            // SAFETY: 写区间位于 self.data 内, 不与其他 CPU 共享
            // (head/tail 仅协调元数据访问)
            unsafe {
                core::ptr::copy_nonoverlapping(src.as_ptr(), dst.as_ptr() as *mut u8, len);
            }
        } else {
            let first = CAP - start;
            // SAFETY: 同上
            unsafe {
                core::ptr::copy_nonoverlapping(
                    src.as_ptr(),
                    self.data.as_ptr().add(start) as *mut u8,
                    first,
                );
                core::ptr::copy_nonoverlapping(
                    src.as_ptr().add(first),
                    self.data.as_ptr() as *mut u8,
                    len - first,
                );
            }
        }
    }

    /// 弹出一段字节到 dst, 返回拷贝字节数 (0 = 空)
    pub fn pop_into(&self, dst: &mut [u8]) -> usize {
        let t = self.tail.load(Ordering::Relaxed);
        let h = self.head.load(Ordering::Acquire);
        let avail = h.wrapping_sub(t);
        if avail == 0 {
            return 0;
        }
        let n = avail.min(dst.len());
        self.read_into(dst, t, n);
        self.tail.store(t.wrapping_add(n), Ordering::Release);
        n
    }

    /// 把 [`abs_off`, `abs_off+n`) 读到 dst
    fn read_into(&self, dst: &mut [u8], abs_off: usize, n: usize) {
        let cap_mask = CAP - 1;
        let start = abs_off & cap_mask;
        if start + n <= CAP {
            // SAFETY: 区间合法
            unsafe {
                core::ptr::copy_nonoverlapping(self.data.as_ptr().add(start), dst.as_mut_ptr(), n);
            }
        } else {
            let first = CAP - start;
            // SAFETY: 区间合法
            unsafe {
                core::ptr::copy_nonoverlapping(
                    self.data.as_ptr().add(start),
                    dst.as_mut_ptr(),
                    first,
                );
                core::ptr::copy_nonoverlapping(
                    self.data.as_ptr(),
                    dst.as_mut_ptr().add(first),
                    n - first,
                );
            }
        }
    }

    /// 直接 peek 一段连续字节 (不消费)
    ///
    /// 仅在 `start..start+len` 全部位于单次环绕内时返回 Some,
    /// 否则返回 None (调用方应分次消费)。
    pub fn peek(&self, abs_off: usize, len: usize) -> Option<(&[u8], usize)> {
        if len == 0 {
            return Some((&[], abs_off));
        }
        let cap_mask = CAP - 1;
        let start = abs_off & cap_mask;
        if start + len > CAP {
            return None;
        }
        // SAFETY: 区间合法
        let slice = unsafe { core::slice::from_raw_parts(self.data.as_ptr().add(start), len) };
        Some((slice, abs_off.wrapping_add(len)))
    }
}

impl<const CAP: usize> Default for RingBuffer<{ CAP }> {
    fn default() -> Self {
        Self::new()
    }
}
