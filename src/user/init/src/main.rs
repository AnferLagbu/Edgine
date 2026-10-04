//! Edgine Init — fork / KPTI 隔离测试 (print_char)

#![no_std]
#![no_main]

use userlib::*;
use userlib::sys::*;

/// KPTI-09 探针目标: 内核镜像基址的**高半区(高别名)映射**。
///
/// 该地址在任何用户视图下都不得可读 —— 读到值即 KPTI 隔离失效。
/// 基址取自各架构链接脚本把内核镜像放在物理 LMA 起点的约定：
/// - x86_64: `KERNEL_BASE(0xFFFF_8000_0000_0000) + 0x100000`
///   （见 `privileged/link/x86_64.ld` 的 `. = 0x100000`，对应
///   `privileged/mm/mod.rs` 的 `KERNEL_BASE`）。
/// - aarch64: `KERNEL_BASE(0xFFFF_0000_0000_0000) + 0x4008_0000`
///   （见 `privileged/link/aarch64.ld` 的 `. = 0x40080000`，对应
///   `privileged/mm/mod.rs` 的 `KERNEL_BASE`）。
#[cfg(target_arch = "x86_64")]
const KERNEL_IMAGE_ALIAS: u64 = 0xFFFF_8000_0000_0000 + 0x10_0000;
#[cfg(target_arch = "aarch64")]
const KERNEL_IMAGE_ALIAS: u64 = 0xFFFF_0000_0000_0000 + 0x4008_0000;

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! { proc_exit(1); }

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    print_char(b'X');
    print_char(b'\n');
    // 第一次 fork：子进程立即退出，父进程 wait4 收割 —— 收割路径 `remove_and_free`
    // 使 `Process::drop` 运行并销毁其地址空间 (走 destroy_page_table)，该批待释放帧
    // 必然滞留 pending 链（远程核尚未追平代），父进程继续执行
    let child1 = fork();
    if child1 == 0 {
        proc_exit(0);
    }
    wait_pid(child1 as i32);
    print_char(b'Y');
    print_char(b'\n');
    // 第二次 fork：子进程退出时再次结算 release_lock，出口排空上一次滞留的 pending 帧，
    // 帧真正归还 PMM，使延迟释放路径被完整执行
    let child2 = fork();
    if child2 == 0 {
        proc_exit(0);
    }
    wait_pid(child2 as i32);
    // KPTI-09: 探针子进程以 EL0 读取内核镜像高半区别名 —— 预期内核
    // (x86_64 #PF / aarch64 同步异常) 终止该子进程, 故该读取**不会返回值**。
    // 父进程以退出码判定: 非 0 ⇒ 隔离生效; 读到值 ⇒ 子进程打印 FAIL 并以 0 退出。
    let probe = fork();
    if probe == 0 {
        // SAFETY: 故意以用户态解引用内核高半区别名。该地址不在用户视图内,
        // 预期触发异常并由内核终止本进程; 此处**不**假设任何可读内容。
        let byte = unsafe { core::ptr::read_volatile(KERNEL_IMAGE_ALIAS as *const u8) };
        // 执行到此 ⇒ 内核高半区可被用户态读取, 隔离失效。
        print("[KPTI] FAIL: kernel high-half readable from EL0, byte=");
        print_dec(i64::from(byte));
        print_char(b'\n');
        proc_exit(0);
    }
    if wait_pid(probe as i32) != 0 {
        print("[KPTI] EL0 kernel high-half access denied (pid=");
        print_dec(probe as i64);
        println(")");
    } else {
        print("[KPTI] FAIL: EL0 kernel high-half access was NOT denied (pid=");
        print_dec(probe as i64);
        println(")");
    }

    // ── APS-05: 双核并发 EL0 验证 ─────────────────────────────────────────
    // fork 一个**不 yield** 的忙等子进程: 父子各自长期占用一核并停留在 EL0
    // (fork 的任务投送路径会把子进程推到空闲次核). 忙等期间只以极低频率发
    // syscall (print_char), 供内核每核有界诊断 `[SMP] EL0 pid=N cpu=M` 锚定
    // "本核确有 EL0 任务在执行" —— syscall 只能由 EL0 任务发起. 内核侧上限
    // 4 行/核, 故此处打印 8 次足以覆盖; 之后静默自增 (仍不 yield, 两核不空闲).
    let busy = fork();
    if busy == 0 {
        busy_wait(b'.');
    }
    busy_wait(b'+');
}

/// 不 yield 的忙等 (APS-05): 仅低频发 syscall (打印 `mark`, 上限 8 次), 之后
/// 静默自增 —— 目的是让本核长期持有可运行用户任务并停留在 EL0.
fn busy_wait(mark: u8) -> ! {
    let mut i: u64 = 0;
    let mut printed: u32 = 0;
    loop {
        i = i.wrapping_add(1);
        if printed < 8 && i.is_multiple_of(4_000_000) {
            print_char(mark);
            printed += 1;
        }
    }
}
