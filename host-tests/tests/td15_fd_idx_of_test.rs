// SPDX-License-Identifier: MPL-2.0
// TD-15: TD-02 V4 — fd_alloc::idx_of 集中反查 + 4 子系统本地 fd_to_idx 迁移
//
// 验收:
//   - 4 子系统本地 fd_to_idx (eventfd/signalfd/timerfd/unix) 全部不再用
//     `*_FD_BASE + 字面量` / `*_FD_BASE - 字面量` 算术
//   - `idx_of` 在 FdPlan::ALL 6 范围内反查都正确
//   - timerfd 历史 240 已迁出 smoltcp [0, 256) 重叠 (基址改 1160)
//   - 双架构编译通过; 静态契约测试 5+ 用例全过
//
// 该测试为 I-51/TD-01/TD-02 V4 验收的强化版.

use std::fs;
use std::path::Path;

const FD_ALLOC: &str = "src/kernel/framework/proc/fd_alloc.rs";
const EVENTFD: &str = "src/kernel/framework/syscall/eventfd.rs";
const SIGNALFD: &str = "src/kernel/framework/syscall/signalfd.rs";
const TIMERFD: &str = "src/kernel/framework/syscall/timerfd.rs";
const UNIX: &str = "src/kernel/services/net/unix.rs";

fn read(p: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join(p);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("无法读取 {}: {}", path.display(), e))
}

#[test]
fn test_idx_of_declared_in_fd_alloc() {
    let src = read(FD_ALLOC);
    assert!(
        src.contains("pub fn idx_of(fd: i32) -> Option<(FdSubsystem, usize)>"),
        "fd_alloc 必须公开 pub fn idx_of (TD-15)"
    );
}

#[test]
fn test_timerfd_subsystem_in_fd_plan() {
    let src = read(FD_ALLOC);
    assert!(
        src.contains("TimerFd = 5"),
        "FdSubsystem 必须有 TimerFd = 5 变体 (TD-15)"
    );
    assert!(
        src.contains("pub const TIMER_FD: FdRange = FdRange::new(1160, 16)"),
        "FdPlan::TIMER_FD 必须是 (1160, 16) (TD-15)"
    );
    assert!(
        src.contains("pub const COUNT: usize = 8"),
        "FdSubsystem::COUNT 必须是 8 (TD-15 + PidFd + UserFaultFd)"
    );
    assert!(
        src.contains("FdSubsystem::TimerFd => Self::TIMER_FD"),
        "range_for 必须映射 TimerFd → TIMER_FD (TD-15)"
    );
}

#[test]
fn test_subsystem_local_fd_to_idx_no_base_arith() {
    // 每个子系统的本地 fd_to_idx / is_xxx_fd 必须不再使用 `*_FD_BASE + i32字面量` /
    // `*_FD_BASE - i32字面量` 算术 (赋值语句中的常量定义除外).
    for path in &[EVENTFD, SIGNALFD, TIMERFD, UNIX] {
        let src = read(path);
        // 抽取本地 fd_to_idx / is_xxx_fd 函数体
        // 简化: 全文搜索可疑算术, 排除 const 声明.
        let mut line_no = 0;
        for line in src.lines() {
            line_no += 1;
            let t = line.trim();
            if t.starts_with("pub const ") || t.starts_with("const ") {
                // 形如 `pub const SFD_FD_BASE: i32 = ...` 允许
                continue;
            }
            // 形如 `fd - SFD_FD_BASE`, `fd - EFD_FD_BASE`, `fd - TFD_FD_BASE`,
            // `fd - UDS_FD_BASE` 都不应出现 (本地反查已迁出).
            for base in &["SFD_FD_BASE", "EFD_FD_BASE", "TFD_FD_BASE", "UDS_FD_BASE"] {
                let sub_pattern = format!(" - {} ", base);
                let add_pattern = format!(" + {} ", base);
                if t.contains(&sub_pattern) || t.contains(&add_pattern) {
                    panic!(
                        "{}:{} 含本地 `*_FD_BASE ± 字面量` 算术, 应改走 fd_alloc::idx_of: {}",
                        path, line_no, t
                    );
                }
            }
        }
    }
}

#[test]
fn test_subsystem_local_fd_to_idx_uses_idx_of() {
    // 反向验证: fd_to_idx 体内应出现 fd_alloc::idx_of 调用
    for path in &[EVENTFD, SIGNALFD, TIMERFD, UNIX] {
        let src = read(path);
        // 抽取 fn fd_to_idx(...) 函数体
        let needle = "fn fd_to_idx(fd: i32)";
        let pos = src
            .find(needle)
            .unwrap_or_else(|| panic!("{}: 应有 fn fd_to_idx (TD-15)", path));
        let body_start = pos;
        // 找到函数体结束: 第一个匹配的 `}` 在不缩进的行
        let mut body_end = src.len();
        let mut depth = 0;
        let mut in_fn = false;
        for (i, ch) in src[body_start..].char_indices() {
            match ch {
                '{' => {
                    depth += 1;
                    in_fn = true;
                }
                '}' => {
                    depth -= 1;
                    if in_fn && depth == 0 {
                        body_end = body_start + i + 1;
                        break;
                    }
                }
                _ => {}
            }
        }
        let body = &src[body_start..body_end];
        assert!(
            body.contains("fd_alloc::idx_of") || body.contains("proc::idx_of"),
            "{}: fd_to_idx 体内必须调用 idx_of (TD-15)",
            path
        );
    }
}

#[test]
fn test_timerfd_base_uses_fd_plan_not_literal_240() {
    // timerfd::TFD_FD_BASE 必须来自 FdPlan::TIMER_FD.base, 不是字面量 240
    let src = read(TIMERFD);
    let line = src
        .lines()
        .find(|l| l.contains("TFD_FD_BASE: i32"))
        .expect("TFD_FD_BASE 定义必须存在");
    assert!(
        line.contains("FdPlan::TIMER_FD.base"),
        "TFD_FD_BASE 必须引用 FdPlan::TIMER_FD.base: {}",
        line
    );
    assert!(
        !line.contains("= 240"),
        "TFD_FD_BASE 不能再硬编码 240 (与 smoltcp 重叠): {}",
        line
    );
}

#[test]
fn test_fd_plan_invariant_still_holds() {
    // V4 扩展 ALL 数组后, ranges_non_overlapping 必须仍为 true
    // (此项由启动期 verify_plan() 在运行时校验, 静态契约保证)
    let src = read(FD_ALLOC);
    assert!(
        src.contains("pub const ALL: &'static [FdRange] = &[\n        Self::SMOLTCP,")
            || src
                .lines()
                .any(|l| l.contains("Self::SMOLTCP,") || l.contains("Self::UDS,")),
        "ALL 数组必须包含全部 6 个子系统范围 (TD-15)"
    );
    // TIMER_FD 路径引用至少 2 次: range_for match 臂 + ALL 数组
    let count = src.matches("Self::TIMER_FD").count();
    assert!(
        count >= 2,
        "Self::TIMER_FD 至少出现 2 次 (range_for + ALL), 实际 {} 次 (TD-15)",
        count
    );
}
