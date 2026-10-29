#![deny(unsafe_code)]
#![deny(missing_docs)]
//! @SAFE: 本文件不含 unsafe 代码。
//!
//! `ProcFS` 核心实现 — functions 层 (E6-8 迁移)
//!
//! 从 privileged/fs/procfs/procfs.rs 迁移而来, 0 unsafe, 纯策略.
//! privileged 层转为 re-export 层.

use crate::privileged::mm::api as pmm_api;
use crate::privileged::mm::api::{SlabCacheInfo, slab_get_cache_infos, slab_get_stats};
use crate::privileged::sync::IrqSpinLock as Mutex;
use core::sync::atomic::{AtomicU32, Ordering};

/// `procfs` 条目表容量
pub const PROCFS_MAX_ENTRIES: usize = 32;
/// `procfs` 条目名最大字节数 (含结束符)
pub const PROCFS_MAX_NAME: usize = 32;

/// 单个 `procfs` 目录项
#[derive(Debug, Clone, Copy)]
pub struct ProcfsEntry {
    /// 条目名 (NUL 结尾)
    pub name: [u8; PROCFS_MAX_NAME],
    /// 关联进程 pid (0 = 全局条目)
    pub pid: u32,
    /// 条目类型 (1 = 目录, 2 = 文件, 3 = 进程, 4 = 只读伪文件)
    pub entry_type: u8,
    /// 槽位是否占用
    pub used: bool,
}

impl ProcfsEntry {
    /// 构造空条目 (const, 供静态初始化)
    pub const fn new() -> Self {
        Self {
            name: [0; PROCFS_MAX_NAME],
            pid: 0,
            entry_type: 0,
            used: false,
        }
    }
}

/// `procfs` 全局数据 (条目表 + 有效条目计数)
pub struct ProcfsData {
    entries: Mutex<[ProcfsEntry; PROCFS_MAX_ENTRIES]>,
    entry_count: AtomicU32,
}

// SAFETY (Framekernel P2.2.3): ProcfsData 全部字段自动 Send + Sync。

impl ProcfsData {
    /// 构造空 `procfs` 数据 (const, 供静态初始化)
    pub const fn new() -> Self {
        Self {
            entries: Mutex::new([const { ProcfsEntry::new() }; PROCFS_MAX_ENTRIES]),
            entry_count: AtomicU32::new(0),
        }
    }

    fn set_name(entry: &mut ProcfsEntry, name: &str) {
        let bytes = name.as_bytes();
        let len = bytes.len().min(PROCFS_MAX_NAME - 1);
        entry.name[..len].copy_from_slice(&bytes[..len]);
        entry.name[len] = 0;
    }

    /// 挂载 `procfs` 并填充固定条目 (成功返回 0)
    pub fn mount(&self, _path: &str) -> i32 {
        let mut entries = self.entries.lock();

        Self::set_name(&mut entries[0], "current");
        entries[0].pid = 0;
        entries[0].entry_type = 2;
        entries[0].used = true;

        Self::set_name(&mut entries[1], "cpuinfo");
        entries[1].pid = 0;
        entries[1].entry_type = 4;
        entries[1].used = true;

        Self::set_name(&mut entries[2], "meminfo");
        entries[2].pid = 0;
        entries[2].entry_type = 4;
        entries[2].used = true;

        Self::set_name(&mut entries[3], "version");
        entries[3].pid = 0;
        entries[3].entry_type = 4;
        entries[3].used = true;

        Self::set_name(&mut entries[4], "uptime");
        entries[4].pid = 0;
        entries[4].entry_type = 4;
        entries[4].used = true;

        Self::set_name(&mut entries[5], "stat");
        entries[5].pid = 0;
        entries[5].entry_type = 4;
        entries[5].used = true;

        Self::set_name(&mut entries[6], "mounts");
        entries[6].pid = 0;
        entries[6].entry_type = 4;
        entries[6].used = true;

        Self::set_name(&mut entries[7], "sys/cpu");
        entries[7].pid = 0;
        entries[7].entry_type = 1;
        entries[7].used = true;

        Self::set_name(&mut entries[8], "sys/memory");
        entries[8].pid = 0;
        entries[8].entry_type = 1;
        entries[8].used = true;

        Self::set_name(&mut entries[9], "sys/config");
        entries[9].pid = 0;
        entries[9].entry_type = 1;
        entries[9].used = true;

        self.entry_count.store(10, Ordering::SeqCst);

        0
    }

    /// 新增进程条目 (成功返回 0, 表满返回 -1)
    pub fn add_process(&self, pid: u32, name: &str) -> i32 {
        let mut entries = self.entries.lock();

        for entry in entries.iter_mut() {
            if !entry.used {
                Self::set_name(entry, name);
                entry.pid = pid;
                entry.entry_type = 3;
                entry.used = true;

                self.entry_count.fetch_add(1, Ordering::SeqCst);
                return 0;
            }
        }

        -1
    }

    /// 移除指定 pid 的进程条目 (成功返回 0, 未找到返回 -1)
    pub fn remove_process(&self, pid: u32) -> i32 {
        let mut entries = self.entries.lock();

        for entry in entries.iter_mut() {
            if entry.used && entry.pid == pid {
                entry.used = false;
                entry.pid = 0;
                self.entry_count.fetch_sub(1, Ordering::SeqCst);
                return 0;
            }
        }

        -1
    }

    /// 读取命名条目内容到 `buf`, 返回写入字节数 (未知条目返回 ENOENT)
    pub fn read(name: &str, buf: &mut [u8]) -> i32 {
        // /proc/cpuinfo
        if name == "cpuinfo" {
            return Self::read_cpuinfo(buf);
        }
        // /proc/meminfo
        if name == "meminfo" {
            return Self::read_meminfo(buf);
        }
        // /proc/version
        if name == "version" {
            return Self::read_version(buf);
        }
        // /proc/uptime
        if name == "uptime" {
            return Self::read_uptime(buf);
        }
        // /proc/stat
        if name == "stat" {
            return Self::read_stat(buf);
        }
        // /proc/slabinfo
        if name == "slabinfo" {
            return Self::read_slabinfo(buf);
        }
        // /proc/fs/dcache
        if name == "fs/dcache" {
            return Self::read_dcache(buf);
        }
        // /proc/fs/unkfs
        if name == "fs/unkfs" {
            return Self::read_unkfs(buf);
        }
        // /proc/mounts
        if name == "mounts" {
            return Self::read_mounts(buf);
        }
        // 进程相关条目 /proc/[pid]/status, /cmdline, /fd, /stat
        if let Some(ret) = Self::read_process_entry(name, buf) {
            return ret;
        }
        if name == "sys/cpu" {
            return Self::read_sys_cpu(buf);
        }
        if name == "sys/memory" {
            return Self::read_sys_memory(buf);
        }
        if name == "current" {
            return Self::read_current(buf);
        }
        if name == "sys/config" {
            return crate::functions::intcast::low_i32_from_usize(
                crate::functions::config::procfs::read_sys_config(buf),
            );
        }
        if name == "sys/config.json" {
            return crate::functions::intcast::low_i32_from_usize(
                crate::functions::config::procfs::read_sys_config_json(buf),
            );
        }
        // TD-09 V2: /proc/sys/klog/sinks — 运行时 sink 列表
        if name == "sys/klog/sinks" {
            return crate::functions::intcast::low_i32_from_usize(
                crate::functions::klog::render_text(buf),
            );
        }
        if name == "sys/klog/sinks.json" {
            return crate::functions::intcast::low_i32_from_usize(
                crate::functions::klog::render_json(buf),
            );
        }

        // 未知 entry → ENOENT (VFS 边界约定, 不要返回裸 -1)
        crate::functions::fs::KernelError::FileNotFound.as_i32()
    }

    /// 将字符串写入 `buf` (超出容量的部分截断, 累计写入位置)
    fn write_str(buf: &mut [u8], pos: &mut usize, s: &str) {
        let b = s.as_bytes();
        let end = (*pos + b.len()).min(buf.len());
        let len = end - *pos;
        buf[*pos..end].copy_from_slice(&b[..len]);
        *pos += len;
    }

    /// 以十进制写入 `u64` (值为 0 时也写一位, 超出容量的部分截断)
    fn write_u64(buf: &mut [u8], pos: &mut usize, val: u64) {
        if val == 0 && *pos < buf.len() {
            buf[*pos] = b'0';
            *pos += 1;
            return;
        }
        let mut tmp = [0u8; 20];
        let mut i = 20;
        let mut v = val;
        while v > 0 && i > 0 {
            i -= 1;
            tmp[i] = (v % 10) as u8 + b'0';
            v /= 10;
        }
        let end = (*pos + (20 - i)).min(buf.len());
        let len = end - *pos;
        buf[*pos..end].copy_from_slice(&tmp[i..i + len]);
        *pos += len;
    }

    /// 读取 `/proc/cpuinfo` 内容
    fn read_cpuinfo(buf: &mut [u8]) -> i32 {
        let mut pos = 0usize;

        let cpu_info = crate::privileged::cpu::get_cpu_info();
        match cpu_info {
            Some(info) => {
                Self::write_str(buf, &mut pos, "processor\t: 0\n");
                Self::write_str(buf, &mut pos, "vendor_id\t: ");
                Self::write_str(buf, &mut pos, info.vendor.name());
                Self::write_str(
                    buf,
                    &mut pos,
                    "\ncpu family\t: 6\nmodel\t\t: 142\nmodel name\t: ",
                );
                Self::write_str(buf, &mut pos, info.brand_name());
                Self::write_str(buf, &mut pos, "\nstepping\t: 10\nmicrocode\t: 0xf0\n");
                Self::write_str(buf, &mut pos, "cpu MHz\t\t: ");
                // 整数商余 (两位小数), 消除内核侧浮点 (见 docs/plan/aarch64-kernel-fp-free.md)
                // 频率取单一时基的登记值 (privileged 顶层公共 API; 旧形态读
                // `CpuInfo` 里的厂商硬编码估计字段, 该字段已随 P-7 删除).
                let hz = crate::privileged::timer::hw_counter_frequency_hz().unwrap_or(0);
                let mhz_int = hz / 1_000_000;
                let mhz_frac = (hz % 1_000_000) / 10_000;
                Self::write_str(buf, &mut pos, &alloc::format!("{mhz_int}.{mhz_frac:02}"));
                Self::write_str(buf, &mut pos, "\ncache size\t: ");
                let cache_kb = info.cache.l1d_size / 1024;
                Self::write_str(buf, &mut pos, &alloc::format!("{cache_kb} KB"));
                Self::write_str(buf, &mut pos, "\nphysical id\t: 0\nsiblings\t: ");
                Self::write_str(
                    buf,
                    &mut pos,
                    &alloc::format!("{}", info.topology.logical_threads),
                );
                Self::write_str(buf, &mut pos, "\ncore id\t\t: 0\ncpu cores\t: ");
                Self::write_str(
                    buf,
                    &mut pos,
                    &alloc::format!("{}", info.topology.physical_cores),
                );
                Self::write_str(buf, &mut pos, "\napicid\t\t: 0\napicilid\t: 0\n");
                Self::write_str(
                    buf,
                    &mut pos,
                    "flags\t\t: fpu vme de pse tsc msr pae mce cx8 apic sep mtrr pge mca cmov pat pse36 clflush mmx fxsr sse sse2 ht syscall nx pdpe1gb rdtscp lm constant_tsc rep_good nopl xtopology cpuid pni pclmulqdq ssse3 fma cx16 pcid sse4_1 sse4_2 x2apic movbe popcnt tsc_deadline_timer aes xsave avx f16c rdrand hypervisor lahf_lm abm invpcid_single\n",
                );
                Self::write_str(buf, &mut pos, "bogomips\t: ");
                // bogomips = MHz × 2, 同样整数商余两位小数
                let bogo_hz = hz * 2;
                let bogo_int = bogo_hz / 1_000_000;
                let bogo_frac = (bogo_hz % 1_000_000) / 10_000;
                Self::write_str(buf, &mut pos, &alloc::format!("{bogo_int}.{bogo_frac:02}"));
                Self::write_str(
                    buf,
                    &mut pos,
                    "\nclflush size\t: 64\ncache_alignment\t: 64\n",
                );
                Self::write_str(
                    buf,
                    &mut pos,
                    "address sizes\t: 46 bits physical, 48 bits virtual\npower management:\n\n",
                );
            }
            None => {
                Self::write_str(
                    buf,
                    &mut pos,
                    "processor\t: 0\nvendor_id\t: Unknown\ncpu family\t: 0\nmodel\t\t: 0\nmodel name\t: Unknown CPU\n",
                );
            }
        }

        crate::functions::intcast::low_i32_from_usize(pos)
    }

    /// 读取 `/proc/meminfo` (内存总量/空闲/使用与 slab 统计)
    fn read_meminfo(buf: &mut [u8]) -> i32 {
        let mut pos = 0usize;
        Self::write_meminfo_memory(buf, &mut pos);
        Self::write_meminfo_slab(buf, &mut pos);
        crate::functions::intcast::low_i32_from_usize(pos)
    }

    /// 写入 `/proc/meminfo` 的内存总量/空闲/使用字段
    fn write_meminfo_memory(buf: &mut [u8], pos: &mut usize) {
        let total = pmm_api::pmm_get_total_pages() * 4; // KB
        let free = pmm_api::pmm_get_free_pages() * 4;
        let used = total - free;

        Self::write_str(buf, pos, "MemTotal:        ");
        Self::write_str(buf, pos, &alloc::format!("{total} kB"));
        Self::write_str(buf, pos, "\nMemFree:         ");
        Self::write_str(buf, pos, &alloc::format!("{free} kB"));
        Self::write_str(buf, pos, "\nMemAvailable:    ");
        Self::write_str(buf, pos, &alloc::format!("{free} kB"));
        Self::write_str(
            buf,
            pos,
            "\nBuffers:         0 kB\nCached:          0 kB\nSwapCached:      0 kB\n",
        );
        Self::write_str(buf, pos, "Active:          ");
        Self::write_str(buf, pos, &alloc::format!("{used} kB"));
        Self::write_str(
            buf,
            pos,
            "\nInactive:        0 kB\nSwapTotal:       0 kB\nSwapFree:        0 kB\n",
        );
        Self::write_str(
            buf,
            pos,
            "Dirty:           0 kB\nWriteback:       0 kB\nAnonPages:       ",
        );
        Self::write_str(buf, pos, &alloc::format!("{used} kB"));
        Self::write_str(
            buf,
            pos,
            "\nMapped:          0 kB\nShmem:           0 kB\nKReclaimable:    0 kB\n",
        );
    }

    /// 写入 `/proc/meminfo` 的 slab/kmalloc 与常量字段
    fn write_meminfo_slab(buf: &mut [u8], pos: &mut usize) {
        let slab = slab_get_stats();
        let slab_kb = (slab.total_memory / 1024) as u64;
        let slab_used_kb = (slab.used_memory / 1024) as u64;
        Self::write_str(buf, pos, "Slab:            ");
        Self::write_str(buf, pos, &alloc::format!("{slab_kb} kB"));
        Self::write_str(buf, pos, "\nSReclaimable:    ");
        Self::write_str(buf, pos, &alloc::format!("{slab_used_kb} kB"));
        Self::write_str(buf, pos, "\nSUnreclaim:      ");
        Self::write_str(
            buf,
            pos,
            &alloc::format!("{} kB", slab_kb.saturating_sub(slab_used_kb)),
        );
        Self::write_str(buf, pos, "\n");
        Self::write_str(
            buf,
            pos,
            "KernelStack:     0 kB\nPageTables:      0 kB\nNFS_Unstable:    0 kB\n",
        );
        Self::write_str(
            buf,
            pos,
            "Bounce:          0 kB\nWritebackTmp:    0 kB\nCommitLimit:     0 kB\n",
        );
        let kmalloc = pmm_api::kmalloc_get_stats();
        let kmalloc_kb = (kmalloc.current_usage / 1024) as u64;
        Self::write_str(
            buf,
            pos,
            "Committed_AS:    0 kB\nVmallocTotal:    0 kB\nVmallocUsed:     ",
        );
        Self::write_str(buf, pos, &alloc::format!("{kmalloc_kb} kB"));
        Self::write_str(buf, pos, "\n");
        Self::write_str(
            buf,
            pos,
            "Percpu:          0 kB\nHardwareCorrupted: 0 kB\nAnonHugePages:   0 kB\n",
        );
        Self::write_str(
            buf,
            pos,
            "ShmemHugePages:  0 kB\nShmemPmdMapped:  0 kB\nFileHugePages:   0 kB\n",
        );
        Self::write_str(
            buf,
            pos,
            "FilePmdMapped:   0 kB\nHugePages_Total: 0\nHugePages_Free:  0\nHugePages_Rsvd:  0\n",
        );
        Self::write_str(
            buf,
            pos,
            "HugePages_Surp:  0\nHugepagesize:    2048 kB\nHugetlb:         0 kB\n",
        );
        Self::write_str(
            buf,
            pos,
            "DirectMap4k:     0 kB\nDirectMap2M:     0 kB\nDirectMap1G:     0 kB\n",
        );
    }

    /// 读取 `/proc/version`
    fn read_version(buf: &mut [u8]) -> i32 {
        let version = "Edgine version 0.1.0 (edgine@build) (rustc 1.78.0) #1 SMP PREEMPT Mon Jul  6 00:00:00 UTC 2026\n";
        let bytes = version.as_bytes();
        let len = bytes.len().min(buf.len());
        buf[..len].copy_from_slice(&bytes[..len]);
        crate::functions::intcast::low_i32_from_usize(len)
    }

    /// 读取 `/proc/uptime`
    fn read_uptime(buf: &mut [u8]) -> i32 {
        let uptime_ms = crate::arch!(timestamp());
        let uptime_secs = uptime_ms / 1000;
        let idle_secs = 0u64;
        let s = alloc::format!("{uptime_secs}.00 {idle_secs} 1\n");
        let bytes = s.as_bytes();
        let len = bytes.len().min(buf.len());
        buf[..len].copy_from_slice(&bytes[..len]);
        crate::functions::intcast::low_i32_from_usize(len)
    }

    /// 读取 `/proc/stat`
    fn read_stat(buf: &mut [u8]) -> i32 {
        let mut pos = 0usize;

        Self::write_str(buf, &mut pos, "cpu  0 0 0 0 0 0 0 0 0 0\n");
        Self::write_str(buf, &mut pos, "cpu0 0 0 0 0 0 0 0 0 0 0\n");
        Self::write_str(
            buf,
            &mut pos,
            "intr 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0\n",
        );
        let ticks = crate::privileged::tick_query::current_tick();
        Self::write_str(buf, &mut pos, "ctxt 0\n");
        Self::write_str(buf, &mut pos, "btime 0\n");
        Self::write_str(buf, &mut pos, "processes 0\n");
        Self::write_str(buf, &mut pos, "procs_running 1\n");
        Self::write_str(buf, &mut pos, "procs_blocked 0\n");
        Self::write_str(
            buf,
            &mut pos,
            &alloc::format!("softirq {ticks} 0 0 0 0 0 0 0 0 0 0\n"),
        );

        crate::functions::intcast::low_i32_from_usize(pos)
    }

    /// 读取 `/proc/slabinfo`
    fn read_slabinfo(buf: &mut [u8]) -> i32 {
        let mut pos = 0usize;

        let mut infos = [SlabCacheInfo {
            object_size: 0,
            total_objects: 0,
            active_objects: 0,
            total_slabs: 0,
        }; 16];
        let count = slab_get_cache_infos(&mut infos);

        Self::write_str(
            buf,
            &mut pos,
            "# <name> <active_objs> <num_objs> <objsize> <objperslab> <pagesperslab>\n",
        );

        for info in infos.iter().take(count) {
            let objperslab = if info.object_size > 0 {
                crate::functions::intcast::low_u32_from_usize(
                    crate::functions::intcast::usize_from_u64(crate::privileged::mm::PAGE_SIZE)
                        / info.object_size as usize,
                )
            } else {
                0
            };
            // pagesperslab: 每个 slab 占用的页数, 由 slab 配置决定
            let pagesperslab = crate::functions::intcast::low_u32_from_usize(
                crate::functions::config::slab::SLAB_DEFAULT_SIZE
                    / crate::functions::intcast::usize_from_u64(crate::privileged::mm::PAGE_SIZE),
            );
            Self::write_str(
                buf,
                &mut pos,
                &alloc::format!(
                    "slab-{} {} {} {} {} {}\n",
                    info.object_size,
                    info.active_objects,
                    info.total_objects,
                    info.object_size,
                    objperslab,
                    pagesperslab,
                ),
            );
        }

        crate::functions::intcast::low_i32_from_usize(pos)
    }

    /// 读取 `/proc/fs/dcache` (dcache/icache 命中率)
    fn read_dcache(buf: &mut [u8]) -> i32 {
        let mut pos = 0usize;

        let (d_hits, d_lookups) = crate::functions::fs::dcache::dcache_hit_rate();
        let (i_hits, i_lookups) = crate::functions::fs::dcache::icache_hit_rate();
        let d_count = crate::functions::fs::dcache::dcache_count();
        let i_count = crate::functions::fs::dcache::icache_count();

        Self::write_str(buf, &mut pos, "dcache_lookups: ");
        Self::write_str(buf, &mut pos, &alloc::format!("{d_lookups}\n"));
        Self::write_str(buf, &mut pos, "dcache_hits: ");
        Self::write_str(buf, &mut pos, &alloc::format!("{d_hits}\n"));
        Self::write_str(buf, &mut pos, "dcache_entries: ");
        Self::write_str(buf, &mut pos, &alloc::format!("{d_count}\n"));
        Self::write_str(buf, &mut pos, "icache_lookups: ");
        Self::write_str(buf, &mut pos, &alloc::format!("{i_lookups}\n"));
        Self::write_str(buf, &mut pos, "icache_hits: ");
        Self::write_str(buf, &mut pos, &alloc::format!("{i_hits}\n"));
        Self::write_str(buf, &mut pos, "icache_entries: ");
        Self::write_str(buf, &mut pos, &alloc::format!("{i_count}\n"));

        crate::functions::intcast::low_i32_from_usize(pos)
    }

    /// 读取 `/proc/fs/unkfs` (UNKFS 池统计)
    fn read_unkfs(buf: &mut [u8]) -> i32 {
        let mut pos = 0usize;

        // 获取 UNKFS 池统计
        let unkfs = crate::functions::fs::unkfs::unkfs::get_unkfs();
        let (allocs, frees, reads, writes) = unkfs.get_stats();

        Self::write_str(buf, &mut pos, "allocs: ");
        Self::write_str(buf, &mut pos, &alloc::format!("{allocs}\n"));
        Self::write_str(buf, &mut pos, "frees: ");
        Self::write_str(buf, &mut pos, &alloc::format!("{frees}\n"));
        Self::write_str(buf, &mut pos, "reads: ");
        Self::write_str(buf, &mut pos, &alloc::format!("{reads}\n"));
        Self::write_str(buf, &mut pos, "writes: ");
        Self::write_str(buf, &mut pos, &alloc::format!("{writes}\n"));

        crate::functions::intcast::low_i32_from_usize(pos)
    }

    /// 读取 `/proc/mounts`
    fn read_mounts(buf: &mut [u8]) -> i32 {
        let mut pos = 0usize;

        Self::write_str(
            buf,
            &mut pos,
            "proc /proc proc rw,nosuid,nodev,noexec,relatime 0 0\n",
        );
        Self::write_str(
            buf,
            &mut pos,
            "devtmpfs /dev devtmpfs rw,nosuid,relatime 0 0\n",
        );
        Self::write_str(
            buf,
            &mut pos,
            "tmpfs /tmp tmpfs rw,nosuid,nodev,relatime 0 0\n",
        );

        crate::functions::intcast::low_i32_from_usize(pos)
    }

    /// 分发 `/proc/[pid]/*` 进程条目读取 (未匹配返回 `None`)
    fn read_process_entry(name: &str, buf: &mut [u8]) -> Option<i32> {
        if !name.starts_with('/') {
            return None;
        }

        // 检查进程状态 /proc/[pid]/status
        if name.ends_with("/status") {
            let pid_str = &name[1..name.len() - 7];
            if let Ok(pid) = pid_str.parse::<u32>() {
                return Some(Self::read_process_status(pid, buf));
            }
        }

        // 检查进程命令行 /proc/[pid]/cmdline
        if name.ends_with("/cmdline") {
            let pid_str = &name[1..name.len() - 8];
            if let Ok(pid) = pid_str.parse::<u32>() {
                return Some(Self::read_process_cmdline(pid, buf));
            }
        }

        // 检查进程文件描述符 /proc/[pid]/fd
        if name.ends_with("/fd") {
            let pid_str = &name[1..name.len() - 3];
            if let Ok(pid) = pid_str.parse::<u32>() {
                return Some(Self::read_process_fd(pid, buf));
            }
        }

        // 检查进程统计 /proc/[pid]/stat
        if name.ends_with("/stat") && !name.ends_with("/status") {
            let pid_str = &name[1..name.len() - 5];
            if let Ok(pid) = pid_str.parse::<u32>() {
                return Some(Self::read_process_stat(pid, buf));
            }
        }

        None
    }

    /// 读取 `/proc/sys/cpu`
    fn read_sys_cpu(buf: &mut [u8]) -> i32 {
        let mut pos = 0usize;

        let cpu_info = crate::privileged::cpu::get_cpu_info();
        if let Some(info) = cpu_info {
            Self::write_str(buf, &mut pos, "CPU: ");
            Self::write_str(buf, &mut pos, info.brand_name());
            Self::write_str(buf, &mut pos, "\nVendor: ");
            Self::write_str(buf, &mut pos, info.vendor.name());
            Self::write_str(buf, &mut pos, "\nCores: ");
            let cores = info.topology.physical_cores;
            if cores >= 10 {
                buf[pos] = (cores / 10) + b'0';
                pos += 1;
            }
            buf[pos] = (cores % 10) + b'0';
            pos += 1;
            Self::write_str(buf, &mut pos, "\n");
        } else {
            let info = b"CPU: x86_64 (Unknown)\nVendor: N/A\nCores: 1\n";
            let len = info.len().min(buf.len());
            buf[..len].copy_from_slice(&info[..len]);
            return crate::functions::intcast::low_i32_from_usize(len);
        }
        crate::functions::intcast::low_i32_from_usize(pos)
    }

    /// 读取 `/proc/sys/memory`
    fn read_sys_memory(buf: &mut [u8]) -> i32 {
        let total = pmm_api::pmm_get_total_pages();
        let free = pmm_api::pmm_get_free_pages();
        let total_mb = total * 4 / 1024;
        let free_mb = free * 4 / 1024;

        let mut pos = 0usize;
        Self::write_str(buf, &mut pos, "Total: ");
        Self::write_u64(buf, &mut pos, total);
        Self::write_str(buf, &mut pos, " pages (");
        Self::write_u64(buf, &mut pos, total_mb);
        Self::write_str(buf, &mut pos, " MB)\nFree:  ");
        Self::write_u64(buf, &mut pos, free);
        Self::write_str(buf, &mut pos, " pages (");
        Self::write_u64(buf, &mut pos, free_mb);
        Self::write_str(buf, &mut pos, " MB)\n");

        crate::functions::intcast::low_i32_from_usize(pos)
    }

    /// 读取 `/proc/current`
    fn read_current(buf: &mut [u8]) -> i32 {
        let info = b"PID: 0\nName: kernel\n";
        let len = info.len().min(buf.len());
        buf[..len].copy_from_slice(&info[..len]);
        crate::functions::intcast::low_i32_from_usize(len)
    }

    /// 按索引枚举目录项, 返回 (名称, pid, 条目类型)
    pub fn readdir(&self, index: usize) -> Option<([u8; 32], u32, u8)> {
        let entries = self.entries.lock();
        let mut count = 0;

        for entry in entries.iter() {
            if entry.used {
                if count == index {
                    let mut name = [0u8; 32];
                    let end = entry.name.iter().position(|&b| b == 0).unwrap_or(32);
                    name[..end].copy_from_slice(&entry.name[..end]);
                    return Some((name, entry.pid, entry.entry_type));
                }
                count += 1;
            }
        }
        None
    }

    /// 当前有效条目数
    pub fn entry_count(&self) -> u32 {
        self.entry_count.load(Ordering::SeqCst)
    }

    /// 读取进程状态 /proc/[pid]/status
    fn read_process_status(pid: u32, buf: &mut [u8]) -> i32 {
        use crate::privileged::proc::api::process_with;

        let result = process_with(pid, |proc| {
            let mut pos = 0usize;
            let write_str = |buf: &mut [u8], pos: &mut usize, s: &str| {
                let b = s.as_bytes();
                let end = (*pos + b.len()).min(buf.len());
                let len = end - *pos;
                buf[*pos..end].copy_from_slice(&b[..len]);
                *pos += len;
            };

            write_str(buf, &mut pos, "Name:\t");
            let name_guard = proc.name.lock();
            write_str(buf, &mut pos, &name_guard);
            drop(name_guard);

            write_str(buf, &mut pos, "\nState:\tR (running)\n");
            write_str(buf, &mut pos, &alloc::format!("\nTgid:\t{pid}\n"));
            write_str(buf, &mut pos, &alloc::format!("Pid:\t{pid}\n"));
            let parent_pid = proc.parent.map_or(0, |p| p.0);
            write_str(buf, &mut pos, &alloc::format!("PPid:\t{parent_pid}\n"));
            write_str(buf, &mut pos, "TracerPid:\t0\n");
            write_str(buf, &mut pos, "Uid:\t0\t0\t0\t0\n");
            write_str(buf, &mut pos, "Gid:\t0\t0\t0\t0\n");
            write_str(buf, &mut pos, "FDSize:\t256\n");
            write_str(buf, &mut pos, "Groups:\t0 \n");
            write_str(buf, &mut pos, &alloc::format!("NStgid:\t{pid}\n"));
            write_str(buf, &mut pos, &alloc::format!("NSpid:\t{pid}\n"));
            write_str(buf, &mut pos, &alloc::format!("NSpgid:\t{pid}\n"));
            write_str(buf, &mut pos, &alloc::format!("NSsid:\t{pid}\n"));
            write_str(
                buf,
                &mut pos,
                "VmPeak:\t   1024 kB\nVmSize:\t   1024 kB\nVmRSS:\t     256 kB\nVmSwap:\t       0 kB\n",
            );
            write_str(buf, &mut pos, "Threads:\t1\n");
            write_str(buf, &mut pos, "SigQ:\t0/30670\n");
            write_str(buf, &mut pos, "SigPnd:\t0000000000000000\n");
            write_str(buf, &mut pos, "SigBlk:\t0000000000000000\n");
            write_str(buf, &mut pos, "SigIgn:\t0000000000000000\n");
            write_str(buf, &mut pos, "SigCgt:\t0000000000000000\n");
            write_str(buf, &mut pos, "CapInh:\t0000000000000000\n");
            write_str(buf, &mut pos, "CapPrm:\t0000000000000000\n");
            write_str(buf, &mut pos, "CapEff:\t0000000000000000\n");
            write_str(buf, &mut pos, "CapBnd:\t0000000000000000\n");
            write_str(buf, &mut pos, "CapAmb:\t0000000000000000\n");
            write_str(buf, &mut pos, "Seccomp:\t0\n");
            write_str(buf, &mut pos, "Seccomp_filters:\t0\n");
            write_str(buf, &mut pos, "Cpus_allowed:\t1\n");
            write_str(buf, &mut pos, "Cpus_allowed_list:\t0\n");
            write_str(buf, &mut pos, "voluntary_ctxt_switches:\t0\n");
            write_str(buf, &mut pos, "nonvoluntary_ctxt_switches:\t0\n");

            crate::functions::intcast::low_i32_from_usize(pos)
        });

        result.unwrap_or(-1)
    }

    /// 读取进程命令行 /proc/[pid]/cmdline
    fn read_process_cmdline(pid: u32, buf: &mut [u8]) -> i32 {
        use crate::privileged::proc::api::process_with;

        let result = process_with(pid, |proc| {
            let name_guard = proc.name.lock();
            let name = name_guard.clone();
            drop(name_guard);

            let bytes = name.as_bytes();
            let len = bytes.len().min(buf.len() - 1);
            buf[..len].copy_from_slice(&bytes[..len]);
            buf[len] = 0;

            crate::functions::intcast::low_i32_from_usize(len + 1)
        });

        result.unwrap_or(-1)
    }

    /// 读取进程文件描述符 /proc/[pid]/fd
    fn read_process_fd(pid: u32, buf: &mut [u8]) -> i32 {
        use crate::privileged::proc::api::process_with;

        let result = process_with(pid, |proc| {
            let fds = proc.fd_table.get_all_fds();
            let mut pos = 0usize;
            let write_str = |buf: &mut [u8], pos: &mut usize, s: &str| {
                let b = s.as_bytes();
                let end = (*pos + b.len()).min(buf.len());
                let len = end - *pos;
                buf[*pos..end].copy_from_slice(&b[..len]);
                *pos += len;
            };

            for (local_fd, global_fd) in &fds {
                write_str(
                    buf,
                    &mut pos,
                    &alloc::format!("{local_fd} -> [{global_fd}]\n"),
                );
            }

            crate::functions::intcast::low_i32_from_usize(pos)
        });

        result.unwrap_or(-1)
    }

    #[expect(
        clippy::no_effect_underscore_binding,
        reason = "no_effect_underscore_binding: let _ = expr 用于类型推导/副作用; 当前优先 expect"
    )]
    /// 读取进程统计 /proc/[pid]/stat
    fn read_process_stat(pid: u32, buf: &mut [u8]) -> i32 {
        use crate::privileged::proc::api::process_with;

        let result = process_with(pid, |proc| {
            let name_guard = proc.name.lock();
            let name = name_guard.clone();
            drop(name_guard);

            // Linux /proc/[pid]/stat 格式 (字段顺序):
            // 进程标识, 进程名, 状态, 父进程, 进程组, 会话, 终端, 前台进程组,
            // 标志, 次缺页, 次缺页累计, 主缺页, 主缺页累计,
            // 用户时间, 系统时间, 子用户时间, 子系统时间, 优先级, nice值,
            // 线程数, 虚拟启动时间, 虚拟内存大小, 常驻集大小
            let state = match proc.state.load(core::sync::atomic::Ordering::SeqCst) {
                0 | 1 | 2 => "R", // Created/Ready/Running 均显示 R
                3 => "S",         // Blocked
                4 => "T",         // Frozen
                5 => "Z",         // Zombie
                _ => "X",         // Terminated
            };

            let parent_pid = proc.parent.map_or(0, |p| p.0);
            let utime = proc.user_time.load(core::sync::atomic::Ordering::SeqCst);
            let stime = proc.sys_time.load(core::sync::atomic::Ordering::SeqCst);
            // 使用 create_time 作为进程启动时间 (ticks)
            let start = crate::privileged::proc::api::proc_get_create_time(pid);
            let vsize = 0u64; // 暂时返回 0
            let _rss = 0u64; // 暂时返回 0

            let s = alloc::format!(
                "{pid} ({name}) {state} {parent_pid} {pid} 0 0 0 0 0 0 {utime} {stime} 0 0 0 {start} 1 0 0 {vsize} 0 0 0 0 0 0 0 0 0 0\n"
            );

            let bytes = s.as_bytes();
            let len = bytes.len().min(buf.len());
            buf[..len].copy_from_slice(&bytes[..len]);
            crate::functions::intcast::low_i32_from_usize(len)
        });

        result.unwrap_or(-1)
    }
}

/// `procfs` 全局数据实例
pub static PROCFS_DATA: ProcfsData = ProcfsData::new();

/// 初始化 `procfs` (条目由 `mount` 填充, 此入口当前为空)
pub fn init() {}
