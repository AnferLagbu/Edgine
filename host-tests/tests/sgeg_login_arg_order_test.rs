//! SGEG 登录 ABI 参数序回归锁 (安全缺口 ⑥)
//!
//! 用户态 `userlib::sys::auth_login(n, p)` 以 a0=note / a1=password 发起登录;
//! 内核 `functions::sgeg::auth::auth_login_syscall` 必须按同一序转发到
//! `privileged::sgeg::pwm_login(note, password)`.
//!
//! 修复前该入口把两个形参命名为 `(password, note)` 再对调转发, 导致内核拿密码串
//! 查用户、拿 note 串验密码 ⇒ `login` 恒失败. 本测试经**真实 syscall 入口**调用,
//! 用真实身份表 + 真实宿主进程验证: 正确序能登录、对调序被拒.
//!
//! 进程夹具沿用 `pivot_root_switch_test` 的形态: 真实 `Process::new` 插入全局
//! `PROCESS_TABLE` 并作为当前任务, Drop 时回收.

use std::ffi::CString;
use std::sync::Mutex;

use edgine::kernel::functions::sgeg::auth::auth_login_syscall;
use edgine::kernel::privileged::proc::{PROCESS_TABLE, Process, Scheduler};
use edgine::kernel::privileged::sgeg::identity;

/// 全局互斥: 本测试二进制内的用例共用 `Scheduler::set_current` 全局当前任务,
/// 并行会互相抹掉, 故串行化.
static GLOBAL: Mutex<()> = Mutex::new(());

/// 临时当前进程: 真实 `Process::new` 插入全局表, Drop 时回收并复位当前任务.
struct TestProc {
    pid: u32,
}

impl TestProc {
    fn new(name: &str) -> Self {
        let pid = PROCESS_TABLE
            .allocate_pid()
            .expect("PROCESS_TABLE.allocate_pid 失败");
        let proc = Box::new(Process::new(pid, name, None));
        PROCESS_TABLE.insert(Box::into_raw(proc));
        Scheduler::set_current(pid);
        Self { pid }
    }
}

impl Drop for TestProc {
    fn drop(&mut self) {
        PROCESS_TABLE.remove_and_free(self.pid);
        Scheduler::set_current(0);
    }
}

/// 正确序 (note, password) 必须登录成功; 对调序必须被拒.
#[test]
fn auth_login_forwards_note_then_password() {
    let _guard = GLOBAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let _proc = TestProc::new("sgeg-login-arg-order");

    // 身份表以 (password, note) 序建条目: note="alice", password="s3cret".
    identity::get_table()
        .create("s3cret", "alice", 0)
        .expect("注册测试身份失败");

    let note = CString::new("alice").expect("note 含 NUL");
    let password = CString::new("s3cret").expect("password 含 NUL");

    // 正确序 note→password: 内核应成功登录并返回 PWM (> 0).
    let ok = auth_login_syscall(note.as_ptr() as u64, password.as_ptr() as u64);
    assert!(
        ok > 0,
        "note/password 正确序应登录成功, 实际返回 {ok} (负值 = 参数序被对调)"
    );

    // 对调序 password→note: note="s3cret" 不存在, 必须失败 (负值).
    let swapped = auth_login_syscall(password.as_ptr() as u64, note.as_ptr() as u64);
    assert!(
        swapped < 0,
        "对调序不应登录成功, 实际返回 {swapped} (说明实参被再次交换)"
    );
}
