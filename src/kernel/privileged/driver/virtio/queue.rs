//! `VirtIO` virtqueue 实现 (`VirtIO` 1.0 split ring).
//!
//! 每个 virtqueue 由三段物理连续的内存区域组成:
//! - 描述符表 (Descriptor Table): 16 字节描述符数组
//! - 可用环 (Available Ring): 驱动→设备的通知环
//! - 已用环 (Used Ring): 设备→驱动的完成环
//!
//! 内存布局遵循 `VirtIO` 1.0 规范第 2.6 节 "Split Virtqueues".

use crate::privileged::mm::{KERNEL_BASE, PAGE_SIZE};

/// virtqueue 项最大数量 (必须为 2 的幂).
pub const VQ_SIZE: u16 = 32;

/// Split virtqueue 描述符.
#[repr(C)]
pub struct VqDesc {
    pub addr: u64,  // 缓冲区客户机物理地址
    pub len: u32,   // 缓冲区长度
    pub flags: u16, // VRING_DESC_F_*
    pub next: u16,  // 下一描述符索引 (链式描述符)
}

/// 可用环头 + 环数组.
#[repr(C)]
pub struct VqAvail {
    pub flags: u16, // VRING_AVAIL_F_NO_INTERRUPT
    pub idx: u16,   // 下一个可用环索引 (驱动递增)
    pub ring: [u16; VQ_SIZE as usize],
    // 当协商 VIRTIO_F_EVENT_IDX 时 used_event 紧跟 ring 之后
}

/// 已用环项.
#[repr(C)]
pub struct VqUsedElem {
    pub id: u32,  // 描述符链头索引
    pub len: u32, // 设备写入的总字节数
}

/// 已用环头 + 环数组.
#[repr(C)]
pub struct VqUsed {
    pub flags: u16, // VRING_USED_F_NO_NOTIFY
    pub idx: u16,   // 下一个已用环索引 (设备递增)
    pub ring: [VqUsedElem; VQ_SIZE as usize],
    // 当协商 VIRTIO_F_EVENT_IDX 时 avail_event 紧跟 ring 之后
}

// ── 描述符标志 ──

pub const VQ_DESC_F_NEXT: u16 = 1; // 链接通过 this.next 继续
pub const VQ_DESC_F_WRITE: u16 = 2; // 设备可写入此缓冲区
pub const VQ_DESC_F_INDIRECT: u16 = 4; // 间接描述符表

// ── 可用环标志 ──

pub const VQ_AVAIL_F_NO_INTERRUPT: u16 = 1;

// ── 已用环标志 ──

pub const VQ_USED_F_NO_NOTIFY: u16 = 1;

/// 单个 virtqueue.
pub struct VirtQueue {
    pub desc: *mut VqDesc,
    pub avail: *mut VqAvail,
    pub used: *mut VqUsed,
    pub queue_size: u16,
    pub free_head: u16,      // 空闲描述符链头
    pub last_used_idx: u16,  // 最近见到的 used ring 索引
    pub next_avail_idx: u16, // 驱动下次使用的 avail ring 索引
    // --- 所有权追踪 (用于 DMA 安全性) ---
    /// 已分配页的物理地址 (用于 `x86_64` 上 phys↔virt 转换).
    pub desc_phys: u64,
    pub avail_phys: u64,
    pub used_phys: u64,
}

// SAFETY: VirtQueue 裸指针指向 PMM 分配的 DMA 页.
// 每个设备实例拥有自己的 virtqueue. 描述符操作使用 volatile 访问与内存屏障.
// SAFETY: VirtQueue 含 MMIO 裸指针, 跨 CPU 安全性由单一所有者 &mut self 访问或外部锁保证.
unsafe impl Send for VirtQueue {}
// SAFETY: 同上, &mut self 或外部锁保证并发安全.
unsafe impl Sync for VirtQueue {}

impl VirtQueue {
    /// 分配并初始化一个 split virtqueue.
    ///
    /// 当 `legacy` 为 true 时, used ring 对齐到 4096 字节边界
    /// (QEMU 旧版 `VirtIO` 传输所要求 — `VIRTIO_PCI_VRING_ALIGN`).
    /// 此时需要 2 个页而不是 1 个.
    // 有意窄化: 用户内存代理, 指针/长度上下文保证
    #[expect(clippy::cast_possible_truncation)]
    pub fn new(legacy: bool) -> Option<Self> {
        let desc_size = VQ_SIZE as usize * core::mem::size_of::<VqDesc>();
        let avail_size = core::mem::size_of::<VqAvail>() + 2 /* event index padding */;
        let used_size  = core::mem::size_of::<VqUsed>() + 2 /* event index padding */;

        // 计算偏移: desc | avail (4 字节对齐) | used
        let desc_off = 0usize;
        let avail_off = desc_off + desc_size;
        let used_off = if legacy {
            PAGE_SIZE as usize // 旧版: used ring 必须页对齐 (QEMU 使用 VIRTIO_PCI_VRING_ALIGN)
        } else {
            align_up(avail_off + avail_size, 4)
        };
        let total_size = align_up(used_off + used_size, PAGE_SIZE as usize);

        let pages = total_size.div_ceil(PAGE_SIZE as usize);
        // SAFETY: C ABI 互操作，函数签名与外部代码约定一致
        #[expect(
            clippy::items_after_statements,
            reason = "item 紧邻使用点声明以便阅读上下文; 移至 scope 顶部会割裂逻辑块, 必要时手动重构"
        )]
        unsafe extern "C" {
            fn pmm_alloc_pages(count: u64) -> *mut u8;
        }
        // SAFETY: extern 函数的参数/返回值类型与 C ABI 声明一致; 调用方保证指针有效
        let mem = unsafe { pmm_alloc_pages(pages as u64) };
        if mem.is_null() {
            return None;
        }

        let mem_phys = mem as u64;
        let mem_virt = (mem_phys + KERNEL_BASE) as *mut u8;

        // SAFETY: 调用方保证指针/类型有效 (详见上下文)
        unsafe {
            core::ptr::write_bytes(mem_virt, 0, total_size);

            let desc_ptr = mem_virt as *mut VqDesc;
            let avail_ptr = mem_virt.add(avail_off as usize) as *mut VqAvail;
            let used_ptr = mem_virt.add(used_off as usize) as *mut VqUsed;

            for i in 0..VQ_SIZE {
                let desc = &mut *desc_ptr.add(i as usize);
                desc.flags = 0;
                desc.next = if i + 1 < VQ_SIZE { i + 1 } else { 0xFFFF };
            }

            // 清零 avail 与 used ring (关键: 设备将读取这些)
            core::ptr::write_bytes(avail_ptr as *mut u8, 0, avail_size);
            core::ptr::write_bytes(used_ptr as *mut u8, 0, used_size);

            Some(Self {
                desc: desc_ptr,
                avail: avail_ptr,
                used: used_ptr,
                queue_size: VQ_SIZE,
                free_head: 0,
                last_used_idx: 0,
                next_avail_idx: 0,
                desc_phys: mem_phys + desc_off as u64,
                avail_phys: mem_phys + avail_off as u64,
                used_phys: mem_phys + used_off as u64,
            })
        }
    }

    /// 获取描述符表的物理地址.
    pub fn desc_paddr(&self) -> u64 {
        self.desc_phys
    }
    /// 获取 available ring 的物理地址.
    pub fn avail_paddr(&self) -> u64 {
        self.avail_phys
    }
    /// 获取 used ring 的物理地址.
    pub fn used_paddr(&self) -> u64 {
        self.used_phys
    }

    /// 准备一个描述符链并返回头索引.
    /// 若无空闲描述符返回 `0xFFFF`.
    /// 对于简单读写操作, 这将创建单描述符链.
    pub fn prepare_desc(&mut self, buf_paddr: u64, buf_len: u32, write: bool) -> u16 {
        if self.free_head == 0xFFFF {
            return 0xFFFF;
        }
        let head = self.free_head;
        // SAFETY: 调用方保证指针/类型有效 (详见上下文)
        unsafe {
            let desc = &mut *self.desc.add(head as usize);
            let next_free = desc.next; // 覆盖前先保存
            desc.addr = buf_paddr;
            desc.len = buf_len;
            desc.flags = if write { VQ_DESC_F_WRITE } else { 0 };
            desc.next = 0;
            // free_head 推进到下一个空闲描述符
            self.free_head = next_free;
        }
        head
    }

    #[expect(
        clippy::borrow_as_ptr,
        reason = "borrow_as_ptr: &var as *const T 是已知安全 (Rust 2024 可用 &raw const; 替换需追改调用点, 当前优先 expect"
    )]
    /// 提交描述符链到设备 (通知设备).
    /// 返回已提交的可用环索引.
    pub fn submit(&mut self, desc_head: u16) -> u16 {
        // SAFETY: 调用方保证指针/类型有效 (详见上下文)
        unsafe {
            core::ptr::write_volatile(
                &mut (*self.avail).ring[self.next_avail_idx as usize % VQ_SIZE as usize],
                desc_head,
            );
        }
        let idx = self.next_avail_idx;
        self.next_avail_idx = self.next_avail_idx.wrapping_add(1);
        idx
    }

    #[expect(
        clippy::borrow_as_ptr,
        reason = "borrow_as_ptr: &var as *const T 是已知安全 (Rust 2024 可用 &raw const; 替换需追改调用点, 当前优先 expect"
    )]
    /// 提交后通知设备 (调用方必须设置 avail->idx 并写 `QueueNotify`).
    pub fn commit_and_kick(&mut self) {
        // SAFETY: 调用方保证指针/类型有效 (详见上下文)
        unsafe {
            // 全内存屏障: 确保描述符与 ring 写入全局可见
            crate::privileged::sync::arch::fence();
            core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
            core::ptr::write_volatile(&mut (*self.avail).idx, self.next_avail_idx);
        }
    }

    /// 检查是否有已用描述符可用, 有则返回.
    // 有意窄化: 硬件字段宽度, 寄存器/MMIO 定义保证
    #[expect(clippy::cast_possible_truncation)]
    #[expect(
        clippy::borrow_as_ptr,
        reason = "borrow_as_ptr: &var as *const T 是已知安全 (Rust 2024 可用 &raw const; 替换需追改调用点, 当前优先 expect"
    )]
    pub fn pop_used(&mut self) -> Option<(u16, u32)> {
        // SAFETY: 调用方保证指针/类型有效 (详见上下文)
        unsafe {
            let used_idx = core::ptr::read_volatile(&(*self.used).idx);
            if self.last_used_idx == used_idx {
                return None;
            }
            let elem = &(*self.used).ring[self.last_used_idx as usize % VQ_SIZE as usize];
            let id = elem.id as u16;
            let len = elem.len;
            self.last_used_idx = self.last_used_idx.wrapping_add(1);
            Some((id, len))
        }
    }

    /// 完成后将描述符归还到空闲链.
    pub fn reclaim_desc(&mut self, head: u16) {
        // SAFETY: 调用方保证指针/类型有效 (详见上下文)
        unsafe {
            let desc = &mut *self.desc.add(head as usize);
            desc.next = self.free_head;
            self.free_head = head;
        }
    }

    /// 将描述符 head 链接到 next 描述符 (设置 NEXT 标志和 next 指针).
    ///
    /// 用于构建多描述符链 (如 virtio-blk 的 header→data→status).
    pub fn link_desc(&mut self, head: u16, next: u16) {
        // SAFETY: head/next 均为有效的描述符索引 (由 prepare_desc 返回),
        // desc 指针指向 PMM 分配的 DMA 页, 生命周期由 VirtQueue 管理.
        unsafe {
            let desc = &mut *self.desc.add(head as usize);
            desc.flags |= VQ_DESC_F_NEXT;
            desc.next = next;
        }
    }
}

// ============================================================================
// DMA 缓冲区 (safe API, 供 functions 层调用)
// ============================================================================

/// DMA 缓冲区: 物理连续的内核页, 供 `VirtIO` 设备 DMA 访问.
///
/// 提供 safe 的字节级读写方法, 使 functions 层无需 unsafe 即可
/// 构造请求头和读写数据.
pub struct DmaBuffer {
    /// 缓冲区物理地址 (设备 DMA 使用)
    phys: u64,
    /// 缓冲区虚拟地址 (内核读写使用)
    virt: *mut u8,
    /// 分配的页数 (Drop 时用于释放, 当前 PMM 未提供释放接口)
    pages: usize,
    /// 缓冲区总大小 (字节)
    size: usize,
}

// SAFETY: DmaBuffer 拥有 PMM 分配的独占页; 单一所有者
unsafe impl Send for DmaBuffer {}

impl DmaBuffer {
    /// 分配指定大小的 DMA 缓冲区.
    ///
    /// 返回 `Some(DmaBuffer)` 表示分配成功, `None` 表示内存不足.
    /// 缓冲区内容初始化为零.
    // 有意窄化: 用户内存代理, 指针/长度上下文保证
    #[expect(clippy::cast_possible_truncation)]
    pub fn new(size: usize) -> Option<Self> {
        let pages = (size + PAGE_SIZE as usize - 1) / PAGE_SIZE as usize;
        // SAFETY: C ABI 互操作，函数签名与外部代码约定一致
        #[expect(
            clippy::items_after_statements,
            reason = "item 紧邻使用点声明以便阅读上下文; 移至 scope 顶部会割裂逻辑块, 必要时手动重构"
        )]
        unsafe extern "C" {
            fn pmm_alloc_pages(count: u64) -> *mut u8;
        }
        // SAFETY: pmm_alloc_pages 由 PMM 模块提供, 参数与返回值类型匹配
        let ptr = unsafe { pmm_alloc_pages(pages as u64) };
        if ptr.is_null() {
            return None;
        }
        let phys = ptr as u64;
        let virt = (phys + KERNEL_BASE) as *mut u8;
        // SAFETY: virt 指向有效内核页, 长度 pages * PAGE_SIZE >= size
        unsafe {
            core::ptr::write_bytes(virt, 0, size);
        }
        Some(Self {
            phys,
            virt,
            pages,
            size,
        })
    }

    /// 物理地址 (用于 `VirtQueue` 描述符)
    #[inline]
    pub fn phys_addr(&self) -> u64 {
        self.phys
    }

    /// 缓冲区总大小
    #[inline]
    pub fn size(&self) -> usize {
        self.size
    }

    /// 从缓冲区 `offset` 处读取一个字节.
    ///
    /// # Panic
    /// `offset >= size` 时 panic.
    /// # Panics
    /// offset 超出缓冲区范围时 panic。
    pub fn read_byte(&self, offset: usize) -> u8 {
        assert!(
            offset < self.size,
            "DmaBuffer::read_byte: offset out of bounds"
        );
        // SAFETY: virt 指向有效内核页, offset < size 已检查
        unsafe { *self.virt.add(offset) }
    }

    /// 向缓冲区 `offset` 处写入一个字节.
    /// # Panics
    /// offset 超出缓冲区范围时 panic。
    pub fn write_byte(&mut self, offset: usize, val: u8) {
        assert!(
            offset < self.size,
            "DmaBuffer::write_byte: offset out of bounds"
        );
        // SAFETY: virt 指向有效内核页, offset < size 已检查, &mut self 保证独占
        unsafe {
            *self.virt.add(offset) = val;
        }
    }

    /// 从缓冲区 `offset` 处读取一个 u32 (小端).
    pub fn read_u32(&self, offset: usize) -> u32 {
        let b0 = u32::from(self.read_byte(offset));
        let b1 = u32::from(self.read_byte(offset + 1));
        let b2 = u32::from(self.read_byte(offset + 2));
        let b3 = u32::from(self.read_byte(offset + 3));
        b0 | (b1 << 8) | (b2 << 16) | (b3 << 24)
    }

    /// 向缓冲区 `offset` 处写入一个 u32 (小端).
    // 有意窄化: 硬件字段宽度, 寄存器/MMIO 定义保证
    #[expect(clippy::cast_possible_truncation)]
    pub fn write_u32(&mut self, offset: usize, val: u32) {
        self.write_byte(offset, val as u8);
        self.write_byte(offset + 1, (val >> 8) as u8);
        self.write_byte(offset + 2, (val >> 16) as u8);
        self.write_byte(offset + 3, (val >> 24) as u8);
    }

    /// 从缓冲区 `offset` 处读取一个 u64 (小端).
    pub fn read_u64(&self, offset: usize) -> u64 {
        let lo = u64::from(self.read_u32(offset));
        let hi = u64::from(self.read_u32(offset + 4));
        lo | (hi << 32)
    }

    /// 向缓冲区 `offset` 处写入一个 u64 (小端).
    // 有意窄化: 资源类型转换, POSIX/Linux ABI 约定
    #[expect(clippy::cast_possible_truncation)]
    pub fn write_u64(&mut self, offset: usize, val: u64) {
        self.write_u32(offset, val as u32);
        self.write_u32(offset + 4, (val >> 32) as u32);
    }

    /// 从 `src` 拷贝 `len` 字节到缓冲区 `offset` 处.
    /// # Panics
    /// 目标范围超出缓冲区大小时 panic。
    pub fn write_slice(&mut self, offset: usize, src: &[u8]) {
        let len = src.len();
        assert!(
            offset + len <= self.size,
            "DmaBuffer::write_slice: out of bounds"
        );
        // SAFETY: virt 指向有效内核页, offset + len <= size 已检查, &mut self 保证独占
        unsafe {
            core::ptr::copy_nonoverlapping(src.as_ptr(), self.virt.add(offset), len);
        }
    }

    /// 从缓冲区 `offset` 处拷贝 `len` 字节到 `dst`.
    /// # Panics
    /// 目标范围超出缓冲区大小时 panic。
    pub fn read_slice(&self, offset: usize, dst: &mut [u8]) {
        let len = dst.len();
        assert!(
            offset + len <= self.size,
            "DmaBuffer::read_slice: out of bounds"
        );
        // SAFETY: virt 指向有效内核页, offset + len <= size 已检查
        unsafe {
            core::ptr::copy_nonoverlapping(self.virt.add(offset), dst.as_mut_ptr(), len);
        }
    }
}

impl Drop for DmaBuffer {
    #[expect(
        clippy::no_effect_underscore_binding,
        reason = "no_effect_underscore_binding: let _ = expr 用于类型推导/副作用; 当前优先 expect"
    )]
    fn drop(&mut self) {
        // 当前 PMM 不提供 pmm_free_pages, 缓冲区生命周期由系统管理.
        // 占位: PMM 增量后接入实际释放. self.pages 记录分配页数供将来使用.
        let _pages = self.pages;
    }
}

/// 将 `val` 向上对齐到 `align` 的下一个倍数.
fn align_up(val: usize, align: usize) -> usize {
    (val + align - 1) & !(align - 1)
}
