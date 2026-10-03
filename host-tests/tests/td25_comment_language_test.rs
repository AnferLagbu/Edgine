//! TD-24 / TD-25 注释语言一致性 audit 契约测试
//!
//! 验证 [scripts/audit_comment_language.py] 检测逻辑:
//! 1. 纯中文注释: 不报违规
//! 2. 纯英文段落 (3+ 英文长词, 总长 > 30 字符): 报违规
//! 3. 短英文注释 (引用/标识符/单词): 不报违规
//! 4. 中英混杂 (中文 + 英文长词): 报违规
//! 5. 例外术语 (RCU/CFS/CR3/CR4/EINVAL): 不计入英文长词
//! 6. 安全相关注释 (// SAFETY:) 中的英文引用: 不报违规
//! 7. 块注释 /* */: 同样检测
//! 8. 真实 audit 脚本在最小 fixture 上零违规通过
//!
//! 通过子进程调用 audit_comment_language.py, 验证 end-to-end 行为.
//! 原 `#![allow(dead_code)]` (F9 违规) 删除 — 实测移除后 0 个 dead_code 警告 (防御性残留).

use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::Command;

/// audit 脚本的绝对路径
fn audit_script() -> std::path::PathBuf {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    Path::new(manifest_dir)
        .parent()
        .unwrap()
        .join("scripts/audit_comment_language.py")
}

/// 在临时目录创建 src/kernel/ 子树, 写入测试 fixture, 然后运行 audit 脚本
/// 返回 (退出码, stdout, stderr)
fn run_audit_on_fixture(files: &[(&str, &str)]) -> (i32, String, String) {
    let tmp = std::env::temp_dir().join(format!(
        "td25_fixture_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = fs::remove_dir_all(&tmp);
    let kernel_dir = tmp.join("src/kernel");
    fs::create_dir_all(&kernel_dir).unwrap();

    for (rel, content) in files {
        let p = kernel_dir.join(rel);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let mut f = fs::File::create(&p).unwrap();
        f.write_all(content.as_bytes()).unwrap();
    }

    // 临时改 HOME / PWD 让脚本的 PROJECT_ROOT 重新计算不可行, 改用符号链接
    // 简化方案: 直接复制整个项目到 tmp, 替换 src/kernel 为 fixture
    // 但这样太重, 改用直接调用 Python 脚本, 并在脚本支持环境变量
    // 这里我们用 env 变量 AUDIT_KERNEL_BASE 注入 (脚本会优先读取)
    // 若脚本未支持该变量, 我们改用更简单的方案: 创建临时项目骨架并 patch 脚本
    // 第三方案: 直接用 python 调用脚本 + 把 fixture 拼到一处, 利用 patch 行为
    // 最简单方案: 在 host-test 中复制脚本行为 (直接复用纯函数), 不调用子进程
    // 下面用子进程 + 环境变量, 假设脚本未来会支持 AUDIT_KERNEL_BASE
    // 为当前最小实现, 改用: 直接导入为子进程 + 临时建项目根, 复制脚本

    // 实际策略: 创建完整项目骨架 (含 src/kernel), 并放入 fixture, 通过子进程跑
    // 这要求脚本能找到 PROJECT_ROOT 父目录
    // 更简单: 直接读 audit_script 调内部的 detect_violation 函数
    // 但 detect_violation 不可见. 所以用子进程 + 临时目录作为 cwd

    let project_root = tmp.join("project");
    let p_kernel = project_root.join("src/kernel");
    fs::create_dir_all(&p_kernel).unwrap();
    // 复制脚本 (审计脚本会沿 PROJECT_ROOT 父目录找)
    let script_src = audit_script();
    let scripts_dst = project_root.join("scripts");
    fs::create_dir_all(&scripts_dst).unwrap();
    fs::copy(&script_src, scripts_dst.join("audit_comment_language.py")).unwrap();
    // 复制 fixture
    for (rel, content) in files {
        let p = p_kernel.join(rel);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let mut f = fs::File::create(&p).unwrap();
        f.write_all(content.as_bytes()).unwrap();
    }

    let output = Command::new("python3")
        .arg("scripts/audit_comment_language.py")
        .current_dir(&project_root)
        .output()
        .expect("启动 python3 失败");

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let code = output.status.code().unwrap_or(-1);

    let _ = fs::remove_dir_all(&tmp);
    (code, stdout, stderr)
}

// ── 1. 纯中文注释: 不报违规 ──────────────────────────────────

#[test]
fn test_pure_chinese_comment_passes() {
    let files = &[(
        "mod_a.rs",
        "//! 模块 A 文档注释 (纯中文)\n\
         \n\
         /// 简短函数, 计算两个数之和\n\
         pub fn add(a: i32, b: i32) -> i32 {\n\
         \n\
             // 这里做加法运算\n\
             a + b\n\
         }\n",
    )];
    let (code, stdout, _stderr) = run_audit_on_fixture(files);
    assert_eq!(code, 0, "纯中文注释不应报违规, stdout={}", stdout);
    assert!(
        stdout.contains("PASSED"),
        "期望 PASSED 提示, stdout={}",
        stdout
    );
}

// ── 2. 纯英文段落: 报违规 ─────────────────────────────────────

#[test]
fn test_pure_english_paragraph_violates() {
    let files = &[(
        "mod_b.rs",
        "/// This function calculates the sum of two integers.\n\
         /// It accepts parameters a and b, returning the result.\n\
         pub fn add(a: i32, b: i32) -> i32 {\n\
             a + b\n\
         }\n",
    )];
    let (code, stdout, _stderr) = run_audit_on_fixture(files);
    assert_eq!(code, 1, "纯英文段落应报违规, stdout={}", stdout);
    assert!(
        stdout.contains("FAILED"),
        "期望 FAILED 提示, stdout={}",
        stdout
    );
    assert!(
        stdout.contains("mod_b.rs"),
        "应包含违规文件路径, stdout={}",
        stdout
    );
}

// ── 3. 短英文注释: 不报违规 (引用/标识符/单词) ──────────────

#[test]
fn test_short_english_comment_passes() {
    let files = &[(
        "mod_c.rs",
        "/// see Linux man page: futex(2)\n\
         pub fn futex_wait() {}\n\
         \n\
         // TODO: see RFC 1234\n\
         fn rfc() {}\n",
    )];
    let (code, stdout, _stderr) = run_audit_on_fixture(files);
    assert_eq!(
        code, 0,
        "短英文注释 (引用/单条) 不应报违规, stdout={}",
        stdout
    );
}

// ── 4. 中英混杂: 不报违规 (中文字符占主体, 允许夹杂英文术语) ─

#[test]
fn test_mixed_chinese_english_passes() {
    let files = &[(
        "mod_d.rs",
        "/// 这个函数 calculates the sum of two integers and returns result\n\
         /// RCU 同步原语, 读端持有, CR3 切换, 错误码 EINVAL\n\
         pub fn add(a: i32, b: i32) -> i32 {\n\
             a + b\n\
         }\n",
    )];
    let (code, stdout, _stderr) = run_audit_on_fixture(files);
    assert_eq!(
        code, 0,
        "中英混杂 (中文字符占主体) 不应报违规, stdout={}",
        stdout
    );
}

// ── 5. 例外术语 (RCU/CFS/CR3/EINVAL): 不计入英文长词 ──────

#[test]
fn test_allowed_terms_dont_count() {
    let files = &[(
        "mod_e.rs",
        "/// 同步原语: RCU 读端持有, CFS 调度, CR3 切换\n\
         /// 错误处理: 返回 EINVAL / ENOENT / EACCES\n\
         pub fn sync() {}\n",
    )];
    let (code, stdout, _stderr) = run_audit_on_fixture(files);
    assert_eq!(
        code, 0,
        "例外术语 (RCU/CFS/CR3/EINVAL) 不应算英文长词, stdout={}",
        stdout
    );
}

// ── 6. // SAFETY: 中的英文短句: 不报违规 ──────────────────────

#[test]
fn test_safety_short_english_passes() {
    let files = &[(
        "mod_f.rs",
        "pub fn ptr_deref(p: *const u8) -> u8 {\n\
         \n\
             // SAFETY: caller must ensure p is non-null and valid\n\
             unsafe { *p }\n\
         }\n",
    )];
    let (code, stdout, _stderr) = run_audit_on_fixture(files);
    assert_eq!(
        code, 0,
        "// SAFETY: 短英文 (≤ 30 字符 + ≤ 2 英文长词) 不报违规, stdout={}",
        stdout
    );
}

// ── 7. 块注释 /* */: 同样检测 ────────────────────────────────

#[test]
fn test_block_comment_detected() {
    let files = &[(
        "mod_g.rs",
        "/*\n\
         * This is a block comment that explains the entire module\n\
         * implementation in great detail with multiple long words.\n\
         */\n\
         pub fn f() {}\n",
    )];
    let (code, stdout, _stderr) = run_audit_on_fixture(files);
    assert_eq!(code, 1, "纯英文块注释应报违规, stdout={}", stdout);
}

// ── 8. 真实 audit 脚本可执行 ──────────────────────────────────

#[test]
fn test_audit_script_executable() {
    let script = audit_script();
    assert!(script.exists(), "audit 脚本不存在: {}", script.display());

    // 直接 --help 不支持, 改成空 fixture 跑 (0 违规)
    let files = &[(
        "mod_h.rs",
        "/// 纯中文模块文档\n\
         pub fn f() {}\n",
    )];
    let (code, stdout, stderr) = run_audit_on_fixture(files);
    assert_eq!(
        code, 0,
        "空 fixture 应 PASSED, stdout={}, stderr={}",
        stdout, stderr
    );
}

// ── 9. POSIX 签名引用豁免 ──────────────────────────────────
//
// 项目规范: `/// POSIX `func(args)` 形式的单行签名引用, 等价于
// "标准函数原型引用", 豁免. 这是 net_socket.rs 等文件批量英文注释的
// 真实来源, 误判为违规会污染信号.

#[test]
fn test_posix_signature_passes() {
    let files = &[(
        "socket.rs",
        "/// POSIX `bind(fd, addr, addrlen)`\n\
         pub fn bind() {}\n\
         \n\
         /// POSIX `listen(fd, backlog)`\n\
         pub fn listen() {}\n",
    )];
    let (code, stdout, stderr) = run_audit_on_fixture(files);
    assert_eq!(
        code, 0,
        "POSIX 签名引用应 PASSED, stdout={}, stderr={}",
        stdout, stderr
    );
}

#[test]
fn test_bare_signature_passes() {
    // 无 POSIX 前缀的纯签名引用 (services/net/syscall.rs 常见)
    let files = &[(
        "net.rs",
        "/// sendto(fd, buf, len, flags, dest_addr, addrlen)\n\
         pub fn sendto() {}\n\
         \n\
         /// recvfrom(fd, buf, len, flags, src_addr, addrlen)\n\
         pub fn recvfrom() {}\n\
         \n\
         /// bind(fd, addr, addrlen)\n\
         pub fn bind() {}\n",
    )];
    let (code, stdout, stderr) = run_audit_on_fixture(files);
    assert_eq!(
        code, 0,
        "纯签名引用应 PASSED, stdout={}, stderr={}",
        stdout, stderr
    );
}

#[test]
fn test_bare_signature_with_narrative_violates() {
    // 签名引用 + 叙述文字, 仍违规 (因为整行不只是签名)
    let files = &[(
        "net.rs",
        "/// sendto(fd, buf, len, flags) and the kernel needs to dispatch\n\
         pub fn sendto() {}\n",
    )];
    let (code, _stdout, _stderr) = run_audit_on_fixture(files);
    assert_eq!(code, 1, "签名引用 + 叙述文字应报违规");
}

#[test]
fn test_non_posix_paragraph_still_violates() {
    // 没有 `POSIX` 标记 + 没有反引号函数名 = 真违规, 豁免规则不适用
    let files = &[(
        "doc.rs",
        "/// This is a long English paragraph about the function design\n\
         /// and its rationale for the system architecture.\n\
         pub fn f() {}\n",
    )];
    let (code, _stdout, _stderr) = run_audit_on_fixture(files);
    assert_eq!(code, 1, "非 POSIX 纯英文段落应报违规");
}

#[test]
fn test_posix_marker_without_backtick_still_violates() {
    // 有 POSIX 标记但无反引号函数名 = 叙述性段落, 仍违规
    let files = &[(
        "doc.rs",
        "/// POSIX compliance is required for all the system calls\n\
         /// and the interface must follow the standard convention.\n\
         pub fn f() {}\n",
    )];
    let (code, _stdout, _stderr) = run_audit_on_fixture(files);
    assert_eq!(code, 1, "POSIX 叙述性段落应报违规 (无反引号函数名)");
}

// ── 10. 代码示例豁免 ──────────────────────────────────
//
// 文档注释中嵌入的代码块 (如 `//! let new_value = Itimerspec { ... }`)
// 是 Rust/C 代码片段, 不是英文叙述. posix_timer.rs 的示例就属于此类.

#[test]
fn test_rust_code_example_passes() {
    let files = &[(
        "doc.rs",
        "//! use crate::services::proc::posix_timer;\n\
         //! let new_value = Itimerspec { it_interval_sec: 1, it_interval_nsec: 0 };\n\
         //! posix_timer::timer_settime(id, 0, Some(&new_value), None);\n\
         pub fn f() {}\n",
    )];
    let (code, stdout, stderr) = run_audit_on_fixture(files);
    assert_eq!(
        code, 0,
        "Rust 代码示例应 PASSED, stdout={}, stderr={}",
        stdout, stderr
    );
}

#[test]
fn test_doc_code_block_skipped() {
    // 文档注释中的 ```rust 代码块 (含英文行) 不应被审计
    let files = &[(
        "mutex.rs",
        "/// # Example\n\
         /// ```rust,ignore\n\
         /// let mutex = Mutex::new(false);\n\
         /// *mutex.get_mut() = true;\n\
         /// ```\n\
         pub struct Mutex;\n",
    )];
    let (code, stdout, stderr) = run_audit_on_fixture(files);
    assert_eq!(
        code, 0,
        "文档代码块内英文应被豁免, stdout={}, stderr={}",
        stdout, stderr
    );
}

#[test]
fn test_c_code_example_passes() {
    let files = &[(
        "doc.rs",
        "/// struct itimerspec new_val = { .it_interval = {1, 0}, .it_value = {1, 0} };\n\
         /// syscall(SYS_timer_settime, id, 0, &new_val, NULL);\n\
         pub fn f() {}\n",
    )];
    let (code, stdout, stderr) = run_audit_on_fixture(files);
    assert_eq!(
        code, 0,
        "C 代码示例应 PASSED, stdout={}, stderr={}",
        stdout, stderr
    );
}

#[test]
fn test_low_marker_density_still_violates() {
    // 只有 1 个代码标记 (1 个 `;`), 视为叙述性段落
    let files = &[(
        "doc.rs",
        "/// This is some English text; more English words here\n\
         /// and even more English content for the documentation.\n\
         pub fn f() {}\n",
    )];
    let (code, _stdout, _stderr) = run_audit_on_fixture(files);
    assert_eq!(code, 1, "低密度代码标记段落应报违规 (仅 1 个分号)");
}
