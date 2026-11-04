// SPDX-License-Identifier: MPL-2.0
//! K-07b 批2 execve 运行时回归锁.
//!
//! 口径来源: `docs/plan/multithreading-project.md` K-07b 批2 —— `execve` 运行时
//! 射程: `envp` 传递、exec 路径按进程 `FsContext` 的 cwd/root 解析、归档权限位
//! 保留、独立测试 ELF 与 `[EXEC] PASS` 断言.
//!
//! 批2 收口的三处缺口此前都能静默退化 (exec 照常"成功", 只是语义错了):
//!
//! 1. **envp 丢失** —— functions 只扫 argv, privileged 把 envp 指针数组整段写
//!    NULL; 新进程看到空环境. 现 argv/envp 同一条扫描 + 同一阶段 (旧地址空间
//!    存活时) 复制进新栈, 载荷逐字回读两条环境变量.
//! 2. **路径不按进程视图解析** —— 相对路径不落到 `FsContext.cwd`, 视图绝对
//!    路径不拼 `FsContext.root`; init 用 `cwd` / `root` 两腿分别钉住.
//! 3. **归档权限位丢失** —— ramfs 建文件默认 0644 / 建目录默认 0777, 不 chmod
//!    则归档里的 0755 丢失, execve 的 x 位判定会 EACCES. 现解包按 `entry.mode`
//!    回写, exec 判定要求常规文件至少一位 x (Linux 对 root 也不放宽).
//!
//! 真跑判据分两条腿: 载体腿 (`scripts/initramfs_boot_test.sh`) 要求 `[EXEC] PASS`
//! 且无 `[EXEC] FAIL`; 默认构建 (空 ramfs、无 cpio 解包) 由 `scripts/qemu_boot_test.sh`
//! 要求 `[EXEC] SKIP: no carrier` 且无 `[EXEC] FAIL` —— A-14 把「无载体」钉成显式
//! 第三态, 预期失败不再与真回归在日志里同形.

use std::fs;
use std::path::PathBuf;

const EXEC_RS: &str = "src/kernel/functions/proc/exec.rs";
const PROC_OPS_RS: &str = "src/kernel/privileged/proc/proc_ops.rs";
const USER_PROC_RS: &str = "src/kernel/privileged/proc/user_proc.rs";
const INITRAMFS_RS: &str = "src/kernel/functions/fs/initramfs.rs";
const SYS_RS: &str = "src/user/lib/src/sys.rs";
const INIT_MAIN_RS: &str = "src/user/init/src/main.rs";
const EXECTEST_RS: &str = "src/user/exectest/src/main.rs";
const EXECTEST_CARGO: &str = "src/user/exectest/Cargo.toml";
const USER_CARGO: &str = "src/user/Cargo.toml";
const CPIO_PY: &str = "scripts/make_initramfs_cpio.py";
const MAKEFILE: &str = "Makefile";
const BOOT_SH: &str = "scripts/initramfs_boot_test.sh";
const QEMU_BOOT_SH: &str = "scripts/qemu_boot_test.sh";
const VFS_PATH_RS: &str = "src/kernel/functions/fs/vfs_path.rs";
const PATH_RS: &str = "src/kernel/functions/fs/path.rs";
const HANDLE_RS: &str = "src/kernel/functions/fs/handle.rs";

fn repo_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest.parent().unwrap().to_path_buf()
}

fn read_src(rel: &str) -> String {
    let p = repo_root().join(rel);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("无法读取 {}: {}", p.display(), e))
}

/// 去掉整行注释: 锁定项是代码形态, 不是"注释里不许提旧事".
fn code_only(src: &str) -> String {
    src.lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// 压缩连续空白为单空格, 使断言不受换行/缩进/续行影响.
fn norm(src: &str) -> String {
    src.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn assert_contains(hay: &str, needle: &str, why: &str) {
    assert!(
        hay.contains(needle),
        "{why}\n  期望包含: {needle}\n  实际片段: {}",
        hay.chars().take(400).collect::<String>()
    );
}

fn assert_absent(hay: &str, needle: &str, why: &str) {
    assert!(!hay.contains(needle), "{why}\n  不应出现: {needle}");
}

fn count_needle(hay: &str, needle: &str) -> usize {
    hay.matches(needle).count()
}

/// 取 `name` 所在函数从首个 `{` 到配对 `}` 的源码 (已剥注释, 故花括号只来自代码).
fn fn_body(src: &str, name: &str) -> String {
    let start = src
        .find(name)
        .unwrap_or_else(|| panic!("函数 {name} 不存在 (形态漂移?)"));
    let bytes: Vec<char> = src[start..].chars().collect();
    let mut depth = 0usize;
    let mut begun = false;
    let mut out = String::new();
    for c in &bytes {
        out.push(*c);
        match c {
            '{' => {
                depth += 1;
                begun = true;
            }
            '}' if begun => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            _ => {}
        }
    }
    assert!(begun, "函数 {name} 未找到函数体");
    out
}

// ── 1. functions: argv/envp 同源扫描 + 可执行性判定 ────────────────────────

#[test]
fn test_execve_scans_argv_and_envp_with_one_policy() {
    let code = code_only(&read_src(EXEC_RS));
    let scan = fn_body(
        &code,
        "fn scan_user_strvec(base: u64) -> Result<u32, Errno>",
    );
    assert_contains(
        &scan,
        "if base == 0 {",
        "空向量 (NULL 基址) 视为 0 条, 不是 EFAULT",
    );
    assert_contains(&scan, "return Ok(0);", "NULL 基址直接返回空计数");
    assert_contains(
        &scan,
        "if !api::validate_user_ptr(base) {",
        "数组基址必须过用户指针校验",
    );
    assert_contains(
        &scan,
        "let Some(entry) = api::read_u64_from_user(slot) else {",
        "槽位必须经 safe 读取原语取指针 (不可解引用)",
    );
    assert_contains(&scan, "return Err(Errno::Efault);", "非法指针一律 EFAULT");
    assert_absent(
        &scan,
        "*slot",
        "scan 不得直接解引用用户指针 (I4: 用户内存只能经 safe 代理)",
    );

    let execve = fn_body(
        &code,
        "pub fn execve_syscall(path: u64, argv: u64, envp: u64)",
    );
    assert_contains(
        &execve,
        "let arg_count = scan_user_strvec(argv)?;",
        "argv 走统一扫描",
    );
    assert_contains(
        &execve,
        "let env_count = scan_user_strvec(envp)?;",
        "envp 必须走同一条扫描 (批2 前 envp 被整体忽略)",
    );
}

#[test]
fn test_execve_requires_exec_permission_bit() {
    let code = code_only(&read_src(EXEC_RS));
    let execve = fn_body(
        &code,
        "pub fn execve_syscall(path: u64, argv: u64, envp: u64)",
    );
    // stat 结果要落内核栈, 不能走 `vfs_stat_internal` 的用户写回入口 (I4 代理会
    // 把高半区内核地址判非法), 故必须经 `vfs_stat_safe` 的内核侧计算口径.
    assert_contains(
        &execve,
        "vfs_stat_safe(path as *const u8, current_pwm)",
        "exec 前必须 stat 目标 (判定可执行性)",
    );
    assert_contains(
        &execve,
        "st.file_type != crate::functions::fs::VfsFileType::File.as_u8()",
        "非常规文件 (目录/设备等) 不可 exec",
    );
    assert_contains(
        &execve,
        "(st.perm & 0o111) == 0",
        "常规文件须至少一位 x (Linux 对 root 也不放宽)",
    );
    assert_contains(&execve, "return Err(Errno::Eacces);", "无 x 位 → EACCES");
    let flat = norm(&execve);
    assert_contains(
        &flat,
        "st.perm & 0o4000",
        "SUID 判定仍保留 (批2 只新增 x 位与类型判定)",
    );
}

#[test]
fn test_execve_passes_envp_to_privileged() {
    let code = code_only(&read_src(EXEC_RS));
    let flat = norm(&code);
    assert_contains(
        &flat,
        "crate::privileged::proc::proc_exec_replace( elf.as_ptr() as u64, elf.len() as u64, argv, arg_count, envp, env_count,",
        "functions 必须把 envp/env_count 一并下传 (6 参 FFI)",
    );
}

// ── 2. privileged: envp 复制发生在旧地址空间存活期间 ───────────────────────

#[test]
fn test_proc_exec_replace_copies_envp_before_switch() {
    let code = code_only(&read_src(PROC_OPS_RS));
    let replace = fn_body(&code, "pub extern \"C\" fn proc_exec_replace");
    assert_contains(&replace, "envp_addr: u64,", "FFI 第 5 参: 旧空间 envp 基址");
    assert_contains(&replace, "env_count: u32,", "FFI 第 6 参: envp 计数");
    let flat = norm(&replace);
    assert_contains(
        &flat,
        "if (argv_addr != 0 && arg_count > 0) || (envp_addr != 0 && env_count > 0) {",
        "仅传 envp 的 exec 也必须触发栈写入",
    );
    assert_contains(
        &flat,
        "envp_addr as *const *const u8, env_count as usize,",
        "envp 指针与计数必须传给栈布置原语",
    );
    // 阶段 2c 必须在 replace_user_space / exec_de_thread 之前 —— 字符串源在旧地址空间.
    let stage2c = replace
        .find("setup_user_stack_in_table")
        .expect("栈布置调用");
    let switch_point = replace
        .find("replace_user_space")
        .expect("地址空间替换调用");
    assert!(
        stage2c < switch_point,
        "envp 复制必须先于地址空间替换 (否则读旧空间即 UAF)"
    );
    let de_thread = replace
        .find("exec_de_thread(current_pid)")
        .expect("de_thread 调用");
    assert!(
        stage2c < de_thread,
        "envp 复制必须先于 de_thread (兄弟线程仍共享旧地址空间)"
    );
}

#[test]
fn test_user_stack_writes_envp_strings_and_sentinel() {
    let code = code_only(&read_src(USER_PROC_RS));
    let setup = fn_body(&code, "pub unsafe fn setup_user_stack_in_table");
    assert_contains(
        &setup,
        "let mut env_lens: alloc::vec::Vec<usize> = alloc::vec::Vec::new();",
        "envp 字符串长度需与 argv 同法先量一遍",
    );
    assert_contains(
        &setup,
        "let ptr_count = 1 + (arg_count + 1) + (env_count + 1);",
        "栈空间必须计入 envp 指针数组 + 哨兵",
    );
    assert_contains(
        &setup,
        "let envp_start_off = pos;",
        "envp 指针区紧随 argv 哨兵之后",
    );
    assert_contains(
        &setup,
        "pos += (env_count + 1) * 8;",
        "envp 指针区按 env_count + 1 预留",
    );
    assert_contains(
        &setup,
        "raw::write_user_u64(cr3, envp_start_off + i * 8, env_str_off as u64);",
        "每个 envp 槽写真实字符串地址 (批2 前整段写 NULL)",
    );
    assert_absent(
        &setup,
        "envp_start_off + i * 8, 0u64",
        "不得再有\"整段写 NULL\"的旧形态",
    );
    assert_contains(
        &setup,
        "raw::write_user_u64(cr3, envp_start_off + env_count * 8, 0u64);",
        "envp 末尾必须有 NULL 哨兵",
    );
    assert_contains(&setup, "env_str_off += l;", "字符串区游标推进");
}

// ── 3. initramfs: 归档权限位保留 ──────────────────────────────────────────

#[test]
fn test_unpack_preserves_archive_mode_bits() {
    let code = code_only(&read_src(INITRAMFS_RS));
    assert_contains(
        &code,
        "let archive_perm = crate::functions::intcast::low_u16_from_u32(entry.mode & 0o7777);",
        "权限位取归档 mode 低 12 位 (类型位由 CPIO_S_IFMT 分支消费)",
    );
    assert_eq!(
        count_needle(
            &code,
            "crate::functions::fs::vfs_chmod(path_buf.as_ptr(), archive_perm, pwm);"
        ),
        2,
        "目录与常规文件两条分支都必须回写归档权限 (0755 里的 x 位) "
    );
    // chmod 必须发生在节点建成之后, 否则 ENOENT 被 `let _ =` 静默吞掉.
    let unpack = fn_body(
        &code,
        "pub fn unpack(data: &[u8]) -> Result<usize, &'static str>",
    );
    let dir_arm = unpack.find("CPIO_S_IFDIR =>").expect("目录分支");
    let reg_arm = unpack.find("CPIO_S_IFREG =>").expect("常规文件分支");
    let chmod_calls: Vec<usize> = unpack
        .match_indices("vfs_chmod(path_buf.as_ptr(), archive_perm, pwm)")
        .map(|(idx, _)| idx)
        .collect();
    assert_eq!(chmod_calls.len(), 2, "两条分支各一次 chmod");
    assert!(dir_arm < chmod_calls[0], "目录 chmod 在 mkdir 之后");
    assert!(reg_arm < chmod_calls[1], "文件 chmod 在落盘之后");
}

// ── 4. userlib: 空向量传 NULL + chroot 包装 ───────────────────────────────

#[test]
fn test_userlib_proc_exec_null_terminates_and_empty_is_null() {
    let code = code_only(&read_src(SYS_RS));
    assert_contains(
        &code,
        "pub const SYS_CHROOT: u64 = 161;",
        "chroot 编号与内核 syscall/types.rs 对齐",
    );
    let exec = fn_body(&code, "pub fn proc_exec(");
    let flat = norm(&exec);
    assert_contains(
        &flat,
        "let argv_ptr = if argv.is_empty() { 0 } else { argv.as_ptr() as u64 };",
        "空 argv 吞切片必须显式传 NULL (空切片 as_ptr 是悬垂非空指针)",
    );
    assert_contains(
        &flat,
        "let envp_ptr = if envp.is_empty() { 0 } else { envp.as_ptr() as u64 };",
        "空 envp 同上, 否则内核按数组解析悬垂指针 → EFAULT",
    );
    // 三处独立比对, 不受 rustfmt 单行/多行折行影响
    assert_contains(&flat, "sys3(", "exec 走三参 syscall 原语");
    assert_contains(&flat, "SYS_EXECVE,", "syscall 编号");
    assert_contains(
        &flat,
        "argv_ptr, envp_ptr",
        "argv/envp 两槽各自有来源 (批2 前 envp 槽没有来源)",
    );

    let chroot = fn_body(&code, "pub fn env_chroot(");
    assert_contains(&chroot, "SYS_CHROOT", "chroot 包装须走 SYS_CHROOT");
}

// ── 5. init: cwd/root 双腿探针 ────────────────────────────────────────────

#[test]
fn test_init_exec_probe_runs_both_path_resolution_legs() {
    let code = code_only(&read_src(INIT_MAIN_RS));
    assert_contains(&code, "fn exec_probe() {", "探针入口");
    assert_contains(
        &code,
        "exec_probe();",
        "启动编排必须调用 (两架构同一条 _start)",
    );
    assert_contains(&code, "const EXEC_LEG_CWD: &[u8] = b\"cwd\";", "cwd 腿标记");
    assert_contains(
        &code,
        "const EXEC_LEG_ROOT: &[u8] = b\"root\";",
        "root 腿标记",
    );

    let child = fn_body(&code, "fn exec_leg_child(leg: &'static [u8]) -> !");
    assert_contains(
        &child,
        "env_chdir(b\"/bin\\0\")",
        "cwd 腿必须切进程工作目录 (FsContext.cwd)",
    );
    assert_contains(
        &child,
        "env_chroot(b\"/bin\\0\")",
        "root 腿必须切进程视图根 (FsContext.root)",
    );
    assert_contains(
        &child,
        "b\"exectest\\0\"",
        "cwd 腿用相对路径 —— 只有按 cwd 解析才命中 /bin/exectest",
    );
    assert_contains(
        &child,
        "b\"/exectest\\0\"",
        "root 腿用视图绝对路径 —— 只有拼 root 前缀才命中 /bin/exectest",
    );
    assert_contains(
        &child,
        "c\"exectest\".as_ptr().cast::<u8>()",
        "argv/环境变量用 C 字符串字面量 (clippy manual_c_str_literals 口径)",
    );
    assert_contains(&child, "c\"K07B_ENV=1\"", "envp 条目一");
    assert_contains(&child, "c\"K07B_BATCH=2\"", "envp 条目二");
    assert_contains(&child, "core::ptr::null(),", "argv/envp 数组须 NULL 结尾");
    assert_contains(
        &child,
        "let exec_rc = proc_exec(path, &argv, &envp);",
        "三参 exec (失败路径要保留返回码供诊断)",
    );
    assert_contains(
        &child,
        "[EXEC] FAIL: view ",
        "视图切换失败必须带腿名与返回码 (父进程只看得到 wstatus)",
    );

    let leg = fn_body(&code, "fn exec_leg(leg: &'static [u8]) -> bool");
    assert_contains(
        &leg,
        "status == 0",
        "父进程按 wstatus 判定 —— 探针失败必须变成探针失败, 不能只看 exec 返回",
    );

    assert_contains(
        &code,
        "println(\"[EXEC] PASS: cwd+root argv+envp\");",
        "两腿全过才打印 PASS (启动门禁按该串断言)",
    );
    assert_contains(
        &code,
        "print(\"[EXEC] FAIL: cwd=\");",
        "任一腿失败必须留下可诊断的失败横幅",
    );

    // A-14 乙: 无载体是显式第三态 —— 载体自检短路在两条腿之前, 不打 FAIL.
    let probe = fn_body(&code, "fn exec_probe()");
    assert_contains(
        &probe,
        "if !carrier_present() {",
        "探针入口先自检载体 (无载体下两腿按设计不可能成功)",
    );
    assert_contains(
        &probe,
        "println(\"[EXEC] SKIP: no carrier\");",
        "无载体打印 SKIP —— scripts/qemu_boot_test.sh 按该串断言 (缺失即判红)",
    );
    assert_contains(&probe, "return;", "SKIP 分支必须短路, 不进两腿");
    assert_eq!(
        count_needle(&probe, "exec_leg("),
        2,
        "exec_probe 只跑 cwd/root 两腿 —— 增减腿数即判据面变化"
    );

    let carrier = fn_body(&code, "fn carrier_present() -> bool");
    assert_contains(
        &carrier,
        "fs_open(b\"/bin/exectest\\0\", O_RDONLY, 0)",
        "载体自检用 open 而非 stat: 用户态无 stat 包装, 且 open 与两腿同走一套路径解析",
    );
    assert_contains(&carrier, "let _ = fs_close(fd);", "探针不得泄漏 fd");
}

// ── 5b. A-14: chdir 与 chroot 的存在性校验不对称 (探针两腿红点位置的成因) ──

/// 取证结论见 `docs/plan/multithreading-project.md` A-14 详情第 4 跳: cwd 腿的红点
/// 落在 `exec` 而不是 `view`, 因为 `chdir` 只校验用户指针就切 cwd; root 腿的红点落在
/// `view`, 因为 `chroot` 有 `require_directory` 前置. 本腿把这条不对称钉成回归锁 ——
/// 若日后给 `chdir` 补存在性校验, 两腿红点会互换, 届时 A-14 详情与载体腿判据都要同步
/// 改, 由本腿提醒, 而不是靠读日志的人猜.
#[test]
fn test_chdir_skips_existence_check_while_chroot_validates() {
    let path_rs = code_only(&read_src(PATH_RS));
    let chdir = fn_body(
        &path_rs,
        "pub fn chdir_syscall(path_ptr: u64) -> Result<usize, Errno>",
    );
    assert_contains(
        &chdir,
        "api::validate_user_ptr(path_ptr)",
        "chdir 校验用户指针 (EFAULT 面)",
    );
    assert_contains(&chdir, "fw::vfs_set_cwd(ptr);", "chdir 直接切 cwd");
    for absent in [
        "require_directory(",
        "vfs_stat",
        "resolve_user_path",
        "Enotdir",
        "Enoent",
    ] {
        assert_absent(
            &chdir,
            absent,
            "chdir 目前不校验路径存在性 —— 该不对称决定探针两腿的红点位置; \
             若已补校验, 需同步改本文件 A-14 详情引用与载体腿判据",
        );
    }

    let chroot = fn_body(
        &path_rs,
        "pub fn chroot_syscall(path_ptr: u64) -> Result<usize, Errno>",
    );
    assert_contains(
        &chroot,
        "require_directory(path_ptr, pwm)?;",
        "chroot 有「存在且为目录」前置 —— 与 chdir 的不对称即在此",
    );
    let req = fn_body(
        &path_rs,
        "fn require_directory(ptr: u64, pwm: u64) -> Result<(), Errno>",
    );
    assert_contains(&req, "fw::vfs_stat_safe(", "require_directory 做真实 stat");
    assert_contains(
        &req,
        "Errno::Enoent",
        "路径不存在返 ENOENT —— 即探针 view 腿 rc=-2 的来源",
    );
    assert_contains(&req, "Errno::Enotdir", "非目录返 ENOTDIR");
}

// ── 5c. A-14: 无载体第三态在启动门禁里有消费腿 (双向 fail-closed) ──────────

/// 消费腿一旦被摘掉, 「无载体」就退回 A-14 的原病灶: 读数无人消费、也没有任何
/// 腿会因它变红. 探针侧的源码锁 (:431 起) 只证明 init 会打这行, 不证明门禁读它,
/// 故门禁脚本面单独钉一次——取法照 `thread_group_k07a_test.rs` 的 `[THREAD]` 先例
/// (按 marker 切段判「两条架构分支各一次」+ 每段 fail-closed).
#[test]
fn test_qemu_boot_gate_consumes_no_carrier_skip_state() {
    let sh = read_src(QEMU_BOOT_SH);
    let parts: Vec<&str> = sh.split(r"\[EXEC\] SKIP:").collect();
    assert_eq!(
        parts.len(),
        3,
        "qemu_boot_test.sh 应在 x86_64 与 aarch64 两条腿各断言一次 `[EXEC] SKIP:` —— \
         少一处即消费腿被摘或只补了单架构 (载体自检链路与架构无关, 两腿必须同形)"
    );
    for (idx, part) in parts.iter().skip(1).enumerate() {
        // 只看该 if 块自身 (截到首个 `fi`): 按固定字符数取窗会越到下一条腿上,
        // 而下一条腿的 `RESULT=1` 足以把「本块被改成非 fail-closed」判成绿.
        let block = part.split("\n            fi\n").next().unwrap_or(*part);
        assert!(
            block.contains("RESULT=1"),
            "第 {idx} 处 `[EXEC] SKIP:` 断言不是 fail-closed (缺失时未置 RESULT=1):\n{}",
            block.chars().take(400).collect::<String>()
        );
    }
    // 双向判据: 只钉 SKIP 会放过「SKIP 与 FAIL 同现」(载体门失效但两腿仍跑),
    // 只钉 FAIL 缺席会放过「init 崩在探针之前、整段没打印」.
    assert_eq!(
        count_needle(&sh, r#"! grep -aq "\[EXEC\] FAIL""#),
        2,
        "两条 SKIP 腿都必须同时判 `[EXEC] FAIL` 缺席 —— 单向断言会让「门失效但仍打 \
         SKIP」或「探针整段没跑」判绿"
    );
}

// ── 6. exectest: 入口 SP 捕获 + 载荷自检 ──────────────────────────────────

#[test]
fn test_exectest_entry_captures_sp_per_arch() {
    let code = code_only(&read_src(EXECTEST_RS));
    assert_contains(&code, "#![no_std]", "裸机载荷");
    assert_contains(&code, "#![no_main]", "入口由汇编提供");
    assert_contains(&code, "\"mov rdi, rsp\",", "x86_64: 入口 SP 作为首参转发");
    assert_contains(&code, "\"call exectest_main\",", "x86_64 转入 Rust 主函数");
    assert_contains(&code, "\"mov x0, sp\",", "aarch64: 入口 SP 作为首参转发");
    assert_contains(&code, "\"bl exectest_main\",", "aarch64 转入 Rust 主函数");
    assert_contains(
        &code,
        "extern \"C\" fn exectest_main(sp: u64) -> !",
        "Rust 侧签名接收入口 SP",
    );
}

#[test]
fn test_exectest_payload_checks_argc_argv_envp_cwd() {
    let code = code_only(&read_src(EXECTEST_RS));
    assert_contains(
        &code,
        "const ENV_SLOT: usize = 5;",
        "argc=3 时 envp 首槽 = 5",
    );
    assert_contains(
        &norm(&code),
        "if unsafe { word_at(sp, 4) } != 0 {",
        "argv NULL 哨兵位置必须钉住 (决定 ENV_SLOT 的正确性)",
    );
    assert_contains(
        &code,
        "env0 == 0 || env1 == 0 || env_end != 0",
        "envp 必须恰好两条且以 NULL 结束",
    );
    assert_contains(&code, "cstr_eq(env0, b\"K07B_ENV=1\")", "环境变量逐字回读");
    assert_contains(&code, "cstr_eq(env1, b\"K07B_BATCH=2\")", "第二条逐字回读");
    assert_contains(&code, "cstr_eq(argv0, b\"exectest\")", "argv[0] 回读");
    assert_contains(&code, "cstr_eq(argv2, b\"K07B\")", "argv[2] 回读");
    assert_contains(&code, "b\"/bin\".as_slice()", "cwd 腿期望 /bin");
    assert_contains(&code, "b\"/\".as_slice()", "root 腿期望 /");
    assert_contains(&code, "env_getcwd(&mut cwd)", "cwd 由 getcwd 回读判定");
    assert_contains(&code, "print(\"[EXEC] PASS: \");", "成功横幅");
    assert_contains(&code, "print(\"[EXEC] FAIL: \");", "失败横幅可定位判据");
}

#[test]
fn test_exectest_crate_is_wired_into_user_workspace() {
    let cargo = read_src(EXECTEST_CARGO);
    assert_contains(&cargo, "name = \"exectest\"", "crate 名");
    assert_contains(
        &cargo,
        "userlib = { path = \"../lib\" }",
        "依赖用户态运行时",
    );
    let workspace = read_src(USER_CARGO);
    assert_contains(
        &workspace,
        "\"exectest\"",
        "必须加入 src/user workspace, 否则 make user 不会构建它",
    );
}

// ── 7. 构建链与门禁: 载荷进归档, 启动脚本断言 PASS/FAIL ───────────────────

#[test]
fn test_cpio_and_makefile_carry_exectest() {
    let py = read_src(CPIO_PY);
    assert_contains(
        &py,
        "(\"bin/exectest\", \"file\", \"exectest.bin\")",
        "归档布局须含 exectest (init 两腿的载荷)",
    );
    let mk = read_src(MAKEFILE);
    assert_contains(
        &mk,
        "USER_EXECTEST_ELF = $(RUST_USER_TARGET)/exectest",
        "ELF 产物变量",
    );
    assert_eq!(
        count_needle(&mk, "exectest.bin: $(USER_EXECTEST_ELF)"),
        2,
        "x86_64 与 aarch64 各一条 .bin 文件规则"
    );
    assert_eq!(
        count_needle(&mk, "$(USER_BUILD_DIR)/exectest.bin"),
        5,
        "引用点 = 2 条 user 依赖 + 2 条归档依赖 + 1 条文件规则目标"
    );
}

#[test]
fn test_boot_gate_asserts_exec_pass_and_no_fail() {
    let sh = read_src(BOOT_SH);
    assert_contains(
        &sh,
        "local marker_exec=\"\\[EXEC\\] PASS: cwd+root\"",
        "门禁必须等 init 父进程的聚合行 (两腿全过) —— 载荷每腿的 PASS 会让轮询提前收尾",
    );
    assert_contains(
        &sh,
        "grep -aq \"\\[EXEC\\] FAIL\" \"$log\"",
        "任一腿失败都必须挡门 (不能只查 PASS 存在)",
    );
    assert_contains(
        &sh,
        "MIN_UNPACKED=7",
        "归档多出 exectest 后条目下限须同步 (8 条目留 1 余量)",
    );
    assert_contains(
        &sh,
        "marker_thread",
        "线程组判据仍保留 (批2 不得削弱批1 断言)",
    );
}

// ── 8. 内核缓冲不得走用户指针入口 (exec 路径 stat/read 的静默失败根因) ────

/// `vfs_stat_safe` / `vfs_read_safe` / `vfs_write_safe` 的入参是**内核**内存
/// (栈/堆, 位于高半区), 不得复用以用户指针为契约的入口:
/// - `vfs_stat_internal` 末尾经 `write_struct_to_user` 写回, 内核地址高于
///   `USER_ADDR_MAX` 必被拒 → stat 恒 -1;
/// - `vfs_read_internal` / `vfs_write_internal` 经 `UserWritePtr` / `UserReadPtr`
///   校验, 同样拒绝内核缓冲 → 内核侧 I/O 恒 -1.
///
/// 这两处曾使 `execve` 在 stat 与读取 ELF 两步静默 ENOENT (chroot 前置的
/// `vfs_stat_safe` 亦同), 而 host 构建不区分内核/用户地址, 运行时不可复现,
/// 只能由源码形态锁 + QEMU 载体腿共同钉住.
#[test]
fn test_kernel_buffers_avoid_user_pointer_entries() {
    let vfs_path = code_only(&read_src(VFS_PATH_RS));

    let core = fn_body(&vfs_path, "pub(crate) fn vfs_stat_core");
    assert_contains(&core, "fs_stat", "核心计算须经 FileSystem::fs_stat");
    assert_contains(&core, "uid_of", "身份映射仍须在核心内完成");

    let stat_internal = fn_body(&vfs_path, "pub extern \"C\" fn vfs_stat_internal");
    assert_contains(
        &stat_internal,
        "vfs_stat_core",
        "用户态入口在核心计算结果之上只做写回",
    );
    assert_contains(
        &stat_internal,
        "write_struct_to_user",
        "用户态入口的写回仍须经 I4 安全代理",
    );

    let stat_safe = fn_body(&vfs_path, "pub fn vfs_stat_safe");
    assert_contains(
        &stat_safe,
        "vfs_stat_core",
        "内核侧 stat 必须走不经用户写回的核心入口",
    );
    assert_absent(
        &stat_safe,
        "vfs_stat_internal",
        "内核栈输出不得交给按用户指针写回的入口",
    );

    let handle = code_only(&read_src(HANDLE_RS));
    let read_safe = fn_body(&handle, "pub fn vfs_read_safe");
    assert_contains(
        &read_safe,
        "vfs_read_kernel",
        "切片必为内核内存, 须走内核缓冲路径",
    );
    assert_absent(
        &read_safe,
        "vfs_read_internal",
        "内核缓冲不得经 UserWritePtr 校验",
    );
    let write_safe = fn_body(&handle, "pub fn vfs_write_safe");
    assert_contains(
        &write_safe,
        "vfs_write_kernel",
        "切片必为内核内存, 须走内核缓冲路径",
    );
    assert_absent(
        &write_safe,
        "vfs_write_internal",
        "内核缓冲不得经 UserReadPtr 校验",
    );
}
