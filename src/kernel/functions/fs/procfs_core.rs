#![deny(unsafe_code)]
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

pub const PROCFS_MAX_ENTRIES: usize = 32;
pub const PROCFS_MAX_NAME: usize = 32;

#[derive(Debug, Clone, Copy)]
pub struct ProcfsEntry {
    pub name: [u8; PROCFS_MAX_NAME],
    pub pid: u32,
    pub entry_type: u8,
    pub used: bool,
}

impl ProcfsEntry {
    pub const fn new() -> Self {
        Self {
            name: [0; PROCFS_MAX_NAME],
            pid: 0,
            entry_type: 0,
            used: false,
        }
    }
}

pub struct ProcfsData {
    entries: Mutex<[ProcfsEntry; PROCFS_MAX_ENTRIES]>,
    entry_count: AtomicU32,
}

// SAFETY (Framekernel P2.2.3): ProcfsData 全部字段自动 Send + Sync。

impl ProcfsData {
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

    #[expect(
        clippy::too_many_lines,
        reason = "函数体超 100 行 (复杂度阈值); 拆分需追改调用链且增加间接层, 当前任务优先 expect 兑底"
    )]
    pub fn read(&self, name: &str, buf: &mut [u8]) -> i32 {
        // /proc/cpuinfo
        if name == "cpuinfo" {
            let mut pos = 0usize;
            let write_str = |buf: &mut [u8], pos: &mut usize, s: &str| {
                let b = s.as_bytes();
                let end = (*pos + b.len()).min(buf.len());
                let len = end - *pos;
                buf[*pos..end].copy_from_slice(&b[..len]);
                *pos += len;
            };

            let cpu_info = crate::privileged::cpu::get_cpu_info();
            match cpu_info {
                Some(info) => {
                    write_str(buf, &mut pos, "processor\t: 0\n");
                    write_str(buf, &mut pos, "vendor_id\t: ");
                    write_str(buf, &mut pos, info.vendor.name());
                    write_str(
                        buf,
                        &mut pos,
                        "\ncpu family\t: 6\nmodel\t\t: 142\nmodel name\t: ",
                    );
                    write_str(buf, &mut pos, info.brand_name());
                    write_str(buf, &mut pos, "\nstepping\t: 10\nmicrocode\t: 0xf0\n");
                    write_str(buf, &mut pos, "cpu MHz\t\t: ");
                    // 整数商余 (两位小数), 消除内核侧浮点 (见 docs/plan/aarch64-kernel-fp-free.md)
                    let hz = info.tsc_frequency_hz;
                    let mhz_int = hz / 1_000_000;
                    let mhz_frac = (hz % 1_000_000) / 10_000;
                    write_str(buf, &mut pos, &alloc::format!("{mhz_int}.{mhz_frac:02}"));
                    write_str(buf, &mut pos, "\ncache size\t: ");
                    let cache_kb = info.cache.l1d_size / 1024;
                    write_str(buf, &mut pos, &alloc::format!("{cache_kb} KB"));
                    write_str(buf, &mut pos, "\nphysical id\t: 0\nsiblings\t: ");
                    write_str(
                        buf,
                        &mut pos,
                        &alloc::format!("{}", info.topology.logical_threads),
                    );
                    write_str(buf, &mut pos, "\ncore id\t\t: 0\ncpu cores\t: ");
                    write_str(
                        buf,
                        &mut pos,
                        &alloc::format!("{}", info.topology.physical_cores),
                    );
                    write_str(buf, &mut pos, "\napicid\t\t: 0\napicilid\t: 0\n");
                    write_str(
                        buf,
                        &mut pos,
                        "flags\t\t: fpu vme de pse tsc msr pae mce cx8 apic sep mtrr pge mca cmov pat pse36 clflush mmx fxsr sse sse2 ht syscall nx pdpe1gb rdtscp lm constant_tsc rep_good nopl xtopology cpuid pni pclmulqdq ssse3 fma cx16 pcid sse4_1 sse4_2 x2apic movbe popcnt tsc_deadline_timer aes xsave avx f16c rdrand hypervisor lahf_lm abm invpcid_single\n",
                    );
                    write_str(buf, &mut pos, "bogomips\t: ");
                    // bogomips = MHz × 2, 同样整数商余两位小数
                    let bogo_hz = hz * 2;
                    let bogo_int = bogo_hz / 1_000_000;
                    let bogo_frac = (bogo_hz % 1_000_000) / 10_000;
                    write_str(buf, &mut pos, &alloc::format!("{bogo_int}.{bogo_frac:02}"));
                    write_str(
                        buf,
                        &mut pos,
                        "\nclflush size\t: 64\ncache_alignment\t: 64\n",
                    );
                    write_str(
                        buf,
                        &mut pos,
                        "address sizes\t: 46 bits physical, 48 bits virtual\npower management:\n\n",
                    );
                }
                None => {
                    write_str(
                        buf,
                        &mut pos,
                        "processor\t: 0\nvendor_id\t: Unknown\ncpu family\t: 0\nmodel\t\t: 0\nmodel name\t: Unknown CPU\n",
                    );
                }
            }

            return pos as i32;
        }

        // /proc/meminfo
        if name == "meminfo" {
            let mut pos = 0usize;
            let write_str = |buf: &mut [u8], pos: &mut usize, s: &str| {
                let b = s.as_bytes();
                let end = (*pos + b.len()).min(buf.len());
                let len = end - *pos;
                buf[*pos..end].copy_from_slice(&b[..len]);
                *pos += len;
            };

            let total = pmm_api::pmm_get_total_pages() * 4; // KB
            let free = pmm_api::pmm_get_free_pages() * 4;
            let used = total - free;

            write_str(buf, &mut pos, "MemTotal:        ");
            write_str(buf, &mut pos, &alloc::format!("{total} kB"));
            write_str(buf, &mut pos, "\nMemFree:         ");
            write_str(buf, &mut pos, &alloc::format!("{free} kB"));
            write_str(buf, &mut pos, "\nMemAvailable:    ");
            write_str(buf, &mut pos, &alloc::format!("{free} kB"));
            write_str(
                buf,
                &mut pos,
                "\nBuffers:         0 kB\nCached:          0 kB\nSwapCached:      0 kB\n",
            );
            write_str(buf, &mut pos, "Active:          ");
            write_str(buf, &mut pos, &alloc::format!("{used} kB"));
            write_str(
                buf,
                &mut pos,
                "\nInactive:        0 kB\nSwapTotal:       0 kB\nSwapFree:        0 kB\n",
            );
            write_str(
                buf,
                &mut pos,
                "Dirty:           0 kB\nWriteback:       0 kB\nAnonPages:       ",
            );
            write_str(buf, &mut pos, &alloc::format!("{used} kB"));
            write_str(
                buf,
                &mut pos,
                "\nMapped:          0 kB\nShmem:           0 kB\nKReclaimable:    0 kB\n",
            );
            let slab = slab_get_stats();
            let slab_kb = (slab.total_memory / 1024) as u64;
            let slab_used_kb = (slab.used_memory / 1024) as u64;
            write_str(buf, &mut pos, "Slab:            ");
            write_str(buf, &mut pos, &alloc::format!("{slab_kb} kB"));
            write_str(buf, &mut pos, "\nSReclaimable:    ");
            write_str(buf, &mut pos, &alloc::format!("{slab_used_kb} kB"));
            write_str(buf, &mut pos, "\nSUnreclaim:      ");
            write_str(
                buf,
                &mut pos,
                &alloc::format!("{} kB", slab_kb.saturating_sub(slab_used_kb)),
            );
            write_str(buf, &mut pos, "\n");
            write_str(
                buf,
                &mut pos,
                "KernelStack:     0 kB\nPageTables:      0 kB\nNFS_Unstable:    0 kB\n",
            );
            write_str(
                buf,
                &mut pos,
                "Bounce:          0 kB\nWritebackTmp:    0 kB\nCommitLimit:     0 kB\n",
            );
            let kmalloc = pmm_api::kmalloc_get_stats();
            let kmalloc_kb = (kmalloc.current_usage / 1024) as u64;
            write_str(
                buf,
                &mut pos,
                "Committed_AS:    0 kB\nVmallocTotal:    0 kB\nVmallocUsed:     ",
            );
            write_str(buf, &mut pos, &alloc::format!("{kmalloc_kb} kB"));
            write_str(buf, &mut pos, "\n");
            write_str(
                buf,
                &mut pos,
                "Percpu:          0 kB\nHardwareCorrupted: 0 kB\nAnonHugePages:   0 kB\n",
            );
            write_str(
                buf,
                &mut pos,
                "ShmemHugePages:  0 kB\nShmemPmdMapped:  0 kB\nFileHugePages:   0 kB\n",
            );
            write_str(
                buf,
                &mut pos,
                "FilePmdMapped:   0 kB\nHugePages_Total: 0\nHugePages_Free:  0\nHugePages_Rsvd:  0\n",
            );
            write_str(
                buf,
                &mut pos,
                "HugePages_Surp:  0\nHugepagesize:    2048 kB\nHugetlb:         0 kB\n",
            );
            write_str(
                buf,
                &mut pos,
                "DirectMap4k:     0 kB\nDirectMap2M:     0 kB\nDirectMap1G:     0 kB\n",
            );

            return pos as i32;
        }

        // /proc/version
        if name == "version" {
            let version = "Edgine version 0.1.0 (edgine@build) (rustc 1.78.0) #1 SMP PREEMPT Mon Jul  6 00:00:00 UTC 2026\n";
            let bytes = version.as_bytes();
            let len = bytes.len().min(buf.len());
            buf[..len].copy_from_slice(&bytes[..len]);
            return len as i32;
        }

        // /proc/uptime
        if name == "uptime" {
            let uptime_ms = crate::arch!(timestamp());
            let uptime_secs = uptime_ms / 1000;
            let idle_secs = 0u64;
            let s = alloc::format!("{uptime_secs}.00 {idle_secs} 1\n");
            let bytes = s.as_bytes();
            let len = bytes.len().min(buf.len());
            buf[..len].copy_from_slice(&bytes[..len]);
            return len as i32;
        }

        // /proc/stat
        if name == "stat" {
            let mut pos = 0usize;
            let write_str = |buf: &mut [u8], pos: &mut usize, s: &str| {
                let b = s.as_bytes();
                let end = (*pos + b.len()).min(buf.len());
                let len = end - *pos;
                buf[*pos..end].copy_from_slice(&b[..len]);
                *pos += len;
            };

            write_str(buf, &mut pos, "cpu  0 0 0 0 0 0 0 0 0 0\n");
            write_str(buf, &mut pos, "cpu0 0 0 0 0 0 0 0 0 0 0\n");
            write_str(
                buf,
                &mut pos,
                "intr 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0\n",
            );
            let ticks = crate::privileged::tick_query::current_tick();
            write_str(buf, &mut pos, "ctxt 0\n");
            write_str(buf, &mut pos, "btime 0\n");
            write_str(buf, &mut pos, "processes 0\n");
            write_str(buf, &mut pos, "procs_running 1\n");
            write_str(buf, &mut pos, "procs_blocked 0\n");
            write_str(
                buf,
                &mut pos,
                &alloc::format!("softirq {ticks} 0 0 0 0 0 0 0 0 0 0\n"),
            );

            return pos as i32;
        }

        // /proc/slabinfo
        if name == "slabinfo" {
            let mut pos = 0usize;
            let write_str = |buf: &mut [u8], pos: &mut usize, s: &str| {
                let b = s.as_bytes();
                let end = (*pos + b.len()).min(buf.len());
                let len = end - *pos;
                buf[*pos..end].copy_from_slice(&b[..len]);
                *pos += len;
            };

            let mut infos = [SlabCacheInfo {
                object_size: 0,
                total_objects: 0,
                active_objects: 0,
                total_slabs: 0,
            }; 16];
            let count = slab_get_cache_infos(&mut infos);

            write_str(
                buf,
                &mut pos,
                "# <name> <active_objs> <num_objs> <objsize> <objperslab> <pagesperslab>\n",
            );

            for info in infos.iter().take(count) {
                let objperslab = if info.object_size > 0 {
                    (crate::privileged::mm::PAGE_SIZE as usize / info.object_size as usize) as u32
                } else {
                    0
                };
                // pagesperslab: 每个 slab 占用的页数, 由 slab 配置决定
                let pagesperslab = (crate::functions::config::slab::SLAB_DEFAULT_SIZE
                    / crate::privileged::mm::PAGE_SIZE as usize)
                    as u32;
                write_str(
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

            return pos as i32;
        }

        // /proc/fs/dcache
        if name == "fs/dcache" {
            let mut pos = 0usize;
            let write_str = |buf: &mut [u8], pos: &mut usize, s: &str| {
                let b = s.as_bytes();
                let end = (*pos + b.len()).min(buf.len());
                let len = end - *pos;
                buf[*pos..end].copy_from_slice(&b[..len]);
                *pos += len;
            };

            let (d_hits, d_lookups) = crate::functions::fs::dcache::dcache_hit_rate();
            let (i_hits, i_lookups) = crate::functions::fs::dcache::icache_hit_rate();
            let d_count = crate::functions::fs::dcache::dcache_count();
            let i_count = crate::functions::fs::dcache::icache_count();

            write_str(buf, &mut pos, "dcache_lookups: ");
            write_str(buf, &mut pos, &alloc::format!("{d_lookups}\n"));
            write_str(buf, &mut pos, "dcache_hits: ");
            write_str(buf, &mut pos, &alloc::format!("{d_hits}\n"));
            write_str(buf, &mut pos, "dcache_entries: ");
            write_str(buf, &mut pos, &alloc::format!("{d_count}\n"));
            write_str(buf, &mut pos, "icache_lookups: ");
            write_str(buf, &mut pos, &alloc::format!("{i_lookups}\n"));
            write_str(buf, &mut pos, "icache_hits: ");
            write_str(buf, &mut pos, &alloc::format!("{i_hits}\n"));
            write_str(buf, &mut pos, "icache_entries: ");
            write_str(buf, &mut pos, &alloc::format!("{i_count}\n"));

            return pos as i32;
        }

        // /proc/fs/unkfs
        if name == "fs/unkfs" {
            let mut pos = 0usize;
            let write_str = |buf: &mut [u8], pos: &mut usize, s: &str| {
                let b = s.as_bytes();
                let end = (*pos + b.len()).min(buf.len());
                let len = end - *pos;
                buf[*pos..end].copy_from_slice(&b[..len]);
                *pos += len;
            };

            // 获取 UNKFS 池统计
            let unkfs = crate::functions::fs::unkfs::unkfs::get_unkfs();
            let (allocs, frees, reads, writes) = unkfs.get_stats();

            write_str(buf, &mut pos, "allocs: ");
            write_str(buf, &mut pos, &alloc::format!("{allocs}\n"));
            write_str(buf, &mut pos, "frees: ");
            write_str(buf, &mut pos, &alloc::format!("{frees}\n"));
            write_str(buf, &mut pos, "reads: ");
            write_str(buf, &mut pos, &alloc::format!("{reads}\n"));
            write_str(buf, &mut pos, "writes: ");
            write_str(buf, &mut pos, &alloc::format!("{writes}\n"));

            return pos as i32;
        }

        // /proc/mounts
        if name == "mounts" {
            let mut pos = 0usize;
            let write_str = |buf: &mut [u8], pos: &mut usize, s: &str| {
                let b = s.as_bytes();
                let end = (*pos + b.len()).min(buf.len());
                let len = end - *pos;
                buf[*pos..end].copy_from_slice(&b[..len]);
                *pos += len;
            };

            write_str(
                buf,
                &mut pos,
                "proc /proc proc rw,nosuid,nodev,noexec,relatime 0 0\n",
            );
            write_str(
                buf,
                &mut pos,
                "devtmpfs /dev devtmpfs rw,nosuid,relatime 0 0\n",
            );
            write_str(
                buf,
                &mut pos,
                "tmpfs /tmp tmpfs rw,nosuid,nodev,relatime 0 0\n",
            );

            return pos as i32;
        }

        // 检查进程状态 /proc/[pid]/status
        if name.starts_with('/') && name.ends_with("/status") {
            let pid_str = &name[1..name.len() - 7];
            if let Ok(pid) = pid_str.parse::<u32>() {
                return self.read_process_status(pid, buf);
            }
        }

        // 检查进程命令行 /proc/[pid]/cmdline
        if name.starts_with('/') && name.ends_with("/cmdline") {
            let pid_str = &name[1..name.len() - 8];
            if let Ok(pid) = pid_str.parse::<u32>() {
                return self.read_process_cmdline(pid, buf);
            }
        }

        // 检查进程文件描述符 /proc/[pid]/fd
        if name.starts_with('/') && name.ends_with("/fd") {
            let pid_str = &name[1..name.len() - 3];
            if let Ok(pid) = pid_str.parse::<u32>() {
                return self.read_process_fd(pid, buf);
            }
        }

        // 检查进程统计 /proc/[pid]/stat
        if name.starts_with('/') && name.ends_with("/stat") && !name.ends_with("/status") {
            let pid_str = &name[1..name.len() - 5];
            if let Ok(pid) = pid_str.parse::<u32>() {
                return self.read_process_stat(pid, buf);
            }
        }

        if name == "sys/cpu" {
            let mut pos = 0usize;
            let write_str = |buf: &mut [u8], pos: &mut usize, s: &str| {
                let b = s.as_bytes();
                let end = (*pos + b.len()).min(buf.len());
                let len = end - *pos;
                buf[*pos..end].copy_from_slice(&b[..len]);
                *pos += len;
            };

            let cpu_info = crate::privileged::cpu::get_cpu_info();
            if let Some(info) = cpu_info {
                write_str(buf, &mut pos, "CPU: ");
                write_str(buf, &mut pos, info.brand_name());
                write_str(buf, &mut pos, "\nVendor: ");
                write_str(buf, &mut pos, info.vendor.name());
                write_str(buf, &mut pos, "\nCores: ");
                let cores = info.topology.physical_cores;
                if cores >= 10 {
                    buf[pos] = (cores / 10) + b'0';
                    pos += 1;
                }
                buf[pos] = (cores % 10) + b'0';
                pos += 1;
                write_str(buf, &mut pos, "\n");
            } else {
                let info = b"CPU: x86_64 (Unknown)\nVendor: N/A\nCores: 1\n";
                let len = info.len().min(buf.len());
                buf[..len].copy_from_slice(&info[..len]);
                return len as i32;
            }
            return pos as i32;
        }

        if name == "sys/memory" {
            let total = pmm_api::pmm_get_total_pages();
            let free = pmm_api::pmm_get_free_pages();
            let total_mb = total * 4 / 1024;
            let free_mb = free * 4 / 1024;

            let mut pos = 0usize;
            let write_str = |buf: &mut [u8], pos: &mut usize, s: &str| {
                let b = s.as_bytes();
                let end = (*pos + b.len()).min(buf.len());
                let len = end - *pos;
                buf[*pos..end].copy_from_slice(&b[..len]);
                *pos += len;
            };
            let write_u64 = |buf: &mut [u8], pos: &mut usize, val: u64| {
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
            };

            write_str(buf, &mut pos, "Total: ");
            write_u64(buf, &mut pos, total);
            write_str(buf, &mut pos, " pages (");
            write_u64(buf, &mut pos, total_mb);
            write_str(buf, &mut pos, " MB)\nFree:  ");
            write_u64(buf, &mut pos, free);
            write_str(buf, &mut pos, " pages (");
            write_u64(buf, &mut pos, free_mb);
            write_str(buf, &mut pos, " MB)\n");

            return pos as i32;
        }

        if name == "current" {
            let info = b"PID: 0\nName: kernel\n";
            let len = info.len().min(buf.len());
            buf[..len].copy_from_slice(&info[..len]);
            return len as i32;
        }

        if name == "sys/config" {
            return crate::functions::config::procfs::read_sys_config(buf) as i32;
        }

        if name == "sys/config.json" {
            return crate::functions::config::procfs::read_sys_config_json(buf) as i32;
        }

        // TD-09 V2: /proc/sys/klog/sinks — 运行时 sink 列表
        if name == "sys/klog/sinks" {
            return crate::functions::klog::render_text(buf) as i32;
        }
        if name == "sys/klog/sinks.json" {
            return crate::functions::klog::render_json(buf) as i32;
        }

        // 未知 entry → ENOENT (VFS 边界约定, 不要返回裸 -1)
        crate::functions::fs::KernelError::FileNotFound.as_i32()
    }

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

    pub fn entry_count(&self) -> u32 {
        self.entry_count.load(Ordering::SeqCst)
    }

    #[expect(
        clippy::unused_self,
        reason = "保留 &self 签名以便调用点统一用法, 不依赖 self 字段时可改关联函数"
    )]
    #[expect(
        clippy::similar_names,
        reason = "变量名相似表达同族概念 (pd/pt/bm 等); 重命名会破坏阅读连续性, 仅在确实混淆时才人工拆分"
    )]
    /// 读取进程状态 /proc/[pid]/status
    fn read_process_status(&self, pid: u32, buf: &mut [u8]) -> i32 {
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
            let ppid = proc.parent.map_or(0, |p| p.0);
            write_str(buf, &mut pos, &alloc::format!("PPid:\t{ppid}\n"));
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

            pos as i32
        });

        result.unwrap_or(-1)
    }

    #[expect(
        clippy::unused_self,
        reason = "保留 &self 签名以便调用点统一用法, 不依赖 self 字段时可改关联函数"
    )]
    /// 读取进程命令行 /proc/[pid]/cmdline
    fn read_process_cmdline(&self, pid: u32, buf: &mut [u8]) -> i32 {
        use crate::privileged::proc::api::process_with;

        let result = process_with(pid, |proc| {
            let name_guard = proc.name.lock();
            let name = name_guard.clone();
            drop(name_guard);

            let bytes = name.as_bytes();
            let len = bytes.len().min(buf.len() - 1);
            buf[..len].copy_from_slice(&bytes[..len]);
            buf[len] = 0;

            (len + 1) as i32
        });

        result.unwrap_or(-1)
    }

    #[expect(
        clippy::unused_self,
        reason = "保留 &self 签名以便调用点统一用法, 不依赖 self 字段时可改关联函数"
    )]
    /// 读取进程文件描述符 /proc/[pid]/fd
    fn read_process_fd(&self, pid: u32, buf: &mut [u8]) -> i32 {
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

            pos as i32
        });

        result.unwrap_or(-1)
    }

    #[expect(
        clippy::unused_self,
        reason = "保留 &self 签名以便调用点统一用法, 不依赖 self 字段时可改关联函数"
    )]
    #[expect(
        clippy::similar_names,
        reason = "变量名相似表达同族概念 (pd/pt/bm 等); 重命名会破坏阅读连续性, 仅在确实混淆时才人工拆分"
    )]
    #[expect(
        clippy::match_same_arms,
        reason = "match_same_arms: match arm 重复是为可读性/调试断点; 当前优先 expect"
    )]
    #[expect(
        clippy::no_effect_underscore_binding,
        reason = "no_effect_underscore_binding: let _ = expr 用于类型推导/副作用; 当前优先 expect"
    )]
    /// 读取进程统计 /proc/[pid]/stat
    fn read_process_stat(&self, pid: u32, buf: &mut [u8]) -> i32 {
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
                0 => "R", // Created
                1 => "R", // Ready
                2 => "R", // Running
                3 => "S", // Blocked
                4 => "T", // Frozen
                5 => "Z", // Zombie
                _ => "X", // Terminated
            };

            let ppid = proc.parent.map_or(0, |p| p.0);
            let utime = proc.user_time.load(core::sync::atomic::Ordering::SeqCst);
            let stime = proc.sys_time.load(core::sync::atomic::Ordering::SeqCst);
            // 使用 create_time 作为进程启动时间 (ticks)
            let start = crate::privileged::proc::api::proc_get_create_time(pid);
            let vsize = 0u64; // 暂时返回 0
            let _rss = 0u64; // 暂时返回 0

            let s = alloc::format!(
                "{pid} ({name}) {state} {ppid} {pid} 0 0 0 0 0 0 {utime} {stime} 0 0 0 {start} 1 0 0 {vsize} 0 0 0 0 0 0 0 0 0 0\n"
            );

            let bytes = s.as_bytes();
            let len = bytes.len().min(buf.len());
            buf[..len].copy_from_slice(&bytes[..len]);
            len as i32
        });

        result.unwrap_or(-1)
    }
}

pub static PROCFS_DATA: ProcfsData = ProcfsData::new();

pub fn init() {}
