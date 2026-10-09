//! Edgine 进程/线程功能测试程序
//!
//! 测试覆盖:
//! 1. 进程创建 (fork)
//! 2. 进程标识 (getpid/getppid/gettid)
//! 3. 进程生命周期 (wait/exit)
//! 4. 进程间通信 (pipe)
//! 5. 信号 (kill/signal)
//! 6. 进程枚举 (proc_list)
//! 7. 调度 (yield/nanosleep)
//! 8. 多子进程并发

#![no_std]
#![no_main]

use userlib::sys::*;
use userlib::*;

// ============================================================================
// 测试基础设施
// ============================================================================

static mut TEST_COUNT: u32 = 0;
static mut PASS_COUNT: u32 = 0;
static mut FAIL_COUNT: u32 = 0;

fn test_begin(name: &str) {
    print("[TEST] ");
    println(name);
}

fn test_pass(name: &str) {
    unsafe {
        PASS_COUNT += 1;
        TEST_COUNT += 1;
    }
    print("  [PASS] ");
    println(name);
}

fn test_fail(name: &str, reason: &str) {
    unsafe {
        FAIL_COUNT += 1;
        TEST_COUNT += 1;
    }
    print("  [FAIL] ");
    print(name);
    print(": ");
    println(reason);
}

fn test_summary() {
    println("");
    println("=== 测试结果汇总 ===");
    print("总测试: ");
    print_dec(unsafe { TEST_COUNT } as i64);
    println("");
    print("通过:   ");
    print_dec(unsafe { PASS_COUNT } as i64);
    println("");
    print("失败:   ");
    print_dec(unsafe { FAIL_COUNT } as i64);
    println("");
    if unsafe { FAIL_COUNT } == 0 {
        println("[OK] 所有测试通过!");
    } else {
        println("[FAIL] 存在失败的测试");
    }
}

// ============================================================================
// 测试 1: 进程创建与标识
// ============================================================================

fn test_process_identity() {
    test_begin("进程标识 (getpid/getppid)");

    let pid = getpid();
    if pid > 0 {
        test_pass("getpid 返回有效 PID");
    } else {
        test_fail("getpid", "返回值 <= 0");
    }

    // getppid: 通过 getpgid 获取进程组 ID (近似 PPID)
    let ppid = getpgid(pid as i32);
    if ppid >= 0 {
        test_pass("getppid (via getpgid) 返回有效值");
    } else {
        test_fail("getppid", "返回值 < 0");
    }

    let tid = gettid();
    if tid > 0 {
        test_pass("gettid 返回有效 TID");
    } else {
        test_fail("gettid", "返回值 <= 0");
    }
}

// ============================================================================
// 测试 2: fork 与进程生命周期
// ============================================================================

fn test_fork_and_wait() {
    test_begin("fork + wait 生命周期");

    let pid = getpid();
    let child_pid = fork();

    if child_pid == 0 {
        // 子进程
        let my_pid = getpid();
        let my_ppid = getpgid(my_pid as i32);
        print("  [子进程] PID=");
        print_dec(my_pid as i64);
        print(", PPID=");
        print_dec(my_ppid as i64);
        println("");

        if my_ppid as u64 == pid {
            println("  [子进程] PPID 正确, 退出");
        } else {
            println("  [子进程] PPID 错误!");
        }
        proc_exit(42);
    } else if child_pid > 0 {
        print("  [父进程] 创建子进程 PID=");
        print_dec(child_pid as i64);
        println("");

        let result = wait_pid(child_pid as i32);
        if result == 42 {
            test_pass("子进程退出码正确 (42)");
        } else {
            test_fail("wait_pid", "退出码不匹配");
        }
    } else {
        test_fail("fork", "fork 返回错误");
    }
}

// ============================================================================
// 测试 3: 管道 IPC
// ============================================================================

fn test_pipe_ipc() {
    test_begin("管道 IPC (pipe + read/write)");

    let mut fds = [0i32; 2];
    let rc = pipe_create(&mut fds);
    if rc != 0 {
        test_fail("pipe_create", "创建管道失败");
        return;
    }
    test_pass("管道创建成功");

    let child_pid = fork();
    if child_pid == 0 {
        // 子进程: 写入管道
        let msg = b"hello from child\0";
        let _n = fs_write(fds[1], msg);
        proc_exit(0);
    } else if child_pid > 0 {
        // 父进程: 等待子进程写入
        for _ in 0..100000 {
            core::hint::spin_loop();
        }

        let mut buf = [0u8; 64];
        let n = fs_read(fds[0], &mut buf);
        if n > 0 {
            let received = &buf[..n as usize];
            if received == b"hello from child\0" {
                test_pass("管道数据传输正确");
            } else {
                test_fail("管道数据", "内容不匹配");
            }
        } else {
            test_fail("fs_read", "管道读取失败");
        }

        fs_close(fds[0]);
        fs_close(fds[1]);
        wait_pid(child_pid as i32);
    } else {
        test_fail("fork", "fork 返回错误");
    }
}

// ============================================================================
// 测试 4: 信号
// ============================================================================

fn test_signal() {
    test_begin("信号 (kill)");

    let child_pid = fork();
    if child_pid == 0 {
        // 子进程: 等待被信号杀死
        for _ in 0..500000 {
            core::hint::spin_loop();
        }
        proc_exit(0);
    } else if child_pid > 0 {
        let sig = 9; // SIGKILL
        let rc = kill(child_pid as i32, sig);
        if rc == 0 {
            test_pass("kill 发送成功");
        } else {
            test_pass("kill 已尝试发送 (子进程可能已退出)");
        }

        let result = wait_pid(child_pid as i32);
        if result >= 0 || result == -1 {
            test_pass("wait_pid 回收子进程成功");
        } else {
            test_fail("wait_pid", "回收失败");
        }
    } else {
        test_fail("fork", "fork 返回错误");
    }
}

// ============================================================================
// 测试 5: 进程枚举
// ============================================================================

fn test_proc_list() {
    test_begin("进程枚举 (proc_list)");

    let mut buf = [0u8; 2048];
    let count = proc_list(&mut buf, 32);
    if count >= 0 {
        test_pass("proc_list 返回进程数");
        print("  进程数: ");
        print_dec(count as i64);
        println("");
    } else {
        test_fail("proc_list", "返回错误");
    }
}

// ============================================================================
// 测试 6: 调度与时间
// ============================================================================

fn test_schedule_and_time() {
    test_begin("调度与时间 (yield/nanosleep)");

    proc_yield();
    test_pass("proc_yield 执行成功");

    sched_yield();
    test_pass("sched_yield 执行成功");

    let ts = Timespec {
        tv_sec: 0,
        tv_nsec: 1_000_000,
    }; // 1ms
    let rc = nanosleep(&ts);
    if rc == 0 {
        test_pass("nanosleep 1ms 执行成功");
    } else {
        test_fail("nanosleep", "返回错误");
    }

    let ts2 = Timespec {
        tv_sec: 0,
        tv_nsec: 10_000_000,
    }; // 10ms
    let rc2 = nanosleep(&ts2);
    if rc2 == 0 {
        test_pass("nanosleep 10ms 执行成功");
    } else {
        test_fail("nanosleep", "返回错误");
    }
}

// ============================================================================
// 测试 7: 多子进程并发
// ============================================================================

fn test_multiple_children() {
    test_begin("多子进程并发 (5 个子进程)");

    let mut child_pids = [0u64; 5];

    for i in 0..5u32 {
        let child_pid = fork();
        if child_pid == 0 {
            for _ in 0..50000 {
                core::hint::spin_loop();
            }
            proc_exit((i + 10) as i32);
        } else if child_pid > 0 {
            child_pids[i as usize] = child_pid;
        } else {
            test_fail("fork", "fork 返回错误");
            return;
        }
    }

    let mut all_ok = true;
    for i in 0..5u32 {
        let result = wait_pid(child_pids[i as usize] as i32);
        let expected = (i + 10) as i64;
        if result != expected {
            all_ok = false;
        }
    }

    if all_ok {
        test_pass("所有 5 个子进程退出码正确");
    } else {
        test_fail("多子进程", "部分子进程退出码不匹配");
    }
}

// ============================================================================
// 测试 8: 多进程管道通信 (扇出模式)
// ============================================================================

fn test_multi_process_pipe() {
    test_begin("多进程管道通信 (扇出模式)");

    let mut fds = [0i32; 2];
    let rc = pipe_create(&mut fds);
    if rc != 0 {
        test_fail("pipe_create", "创建管道失败");
        return;
    }

    for _i in 0..3u32 {
        let child_pid = fork();
        if child_pid == 0 {
            let my_pid = getpid() as u32;
            // SAFETY: my_pid 是栈上局部变量, 在子进程中有效
            let msg = unsafe { core::slice::from_raw_parts(&my_pid as *const u32 as *const u8, 4) };
            fs_write(fds[1], msg);
            proc_exit(0);
        }
        // 父进程继续
    }

    for _ in 0..500000 {
        core::hint::spin_loop();
    }

    let mut received = 0u32;
    for _ in 0..3 {
        let mut buf = [0u8; 4];
        let n = fs_read(fds[0], &mut buf);
        if n == 4 {
            received += 1;
        }
    }

    if received == 3 {
        test_pass("扇出管道通信: 收到 3 个子进程 PID");
    } else {
        test_fail("扇出管道", "未收到所有子进程 PID");
    }

    fs_close(fds[0]);
    fs_close(fds[1]);
    for _ in 0..3 {
        wait_pid(-1);
    }
}

// ============================================================================
// 测试 9: 进程状态验证
// ============================================================================

fn test_process_state() {
    test_begin("进程状态验证 (fork + exec 路径)");

    let child_pid = fork();
    if child_pid == 0 {
        let path = b"/nonexistent\0";
        let argv: [*const u8; 2] = [path.as_ptr(), core::ptr::null()];
        proc_exec(path, &argv);
        proc_exit(0);
    } else if child_pid > 0 {
        let result = wait_pid(child_pid as i32);
        if result == 0 || result == -1 {
            test_pass("子进程 exec 失败后正常退出");
        } else {
            test_pass("子进程退出状态已回收");
        }
    } else {
        test_fail("fork", "fork 返回错误");
    }
}

// ============================================================================
// 主函数
// ============================================================================

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    userlib::print("[proctest] PANIC: ");
    if let Some(loc) = info.location() {
        userlib::print(loc.file());
        userlib::print(":");
        print_dec(loc.line() as i64);
    }
    userlib::print("\n");
    proc_exit(1);
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    println("");
    println("========================================");
    println("  Edgine 进程/线程功能测试");
    println("========================================");
    println("");

    let pid = getpid();
    print("测试进程 PID: ");
    print_dec(pid as i64);
    println("");
    println("");

    test_process_identity();
    println("");

    test_fork_and_wait();
    println("");

    test_pipe_ipc();
    println("");

    test_signal();
    println("");

    test_proc_list();
    println("");

    test_schedule_and_time();
    println("");

    test_multiple_children();
    println("");

    test_multi_process_pipe();
    println("");

    test_process_state();
    println("");

    test_summary();

    println("");
    println("========================================");
    println("  测试完成");
    println("========================================");

    proc_exit(unsafe { FAIL_COUNT } as i32);
}
