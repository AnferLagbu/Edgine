//! D2 双映像 ELF 装载回归锁。
//!
//! 这里锁的是 **Linux `execve` 契约**，不是某个实现细节：
//! - `PT_INTERP` 由 functions 层真读取，主映像不再被改写成内核桩；
//! - 主映像与解释器分别装载，入口交给解释器；
//! - 旧单映像 `proc_exec_replace` 只作为静态包装保留。

const ELF_MOD: &str = include_str!("../../src/kernel/privileged/proc/elf/mod.rs");
const USER_PROC: &str = include_str!("../../src/kernel/privileged/proc/user_proc.rs");
const PROC_OPS: &str = include_str!("../../src/kernel/privileged/proc/proc_ops.rs");
const PROC_API: &str = include_str!("../../src/kernel/privileged/proc/api.rs");
const EXEC: &str = include_str!("../../src/kernel/functions/proc/exec.rs");
const INIT: &str = include_str!("../../src/kernel/functions/init.rs");

#[test]
fn interp_path_is_read_not_rewritten() {
    assert!(
        ELF_MOD.contains("pub fn interp_path(elf_data: &[u8]) -> Option<&str>"),
        "D2 必须从 PT_INTERP 读取路径，而不是只检测前缀"
    );
    assert!(
        !EXEC.contains("prepare_elf_image"),
        "execve 不得再把 PT_INTERP 改写成 elfld"
    );
    assert!(
        !INIT.contains("prepare_elf_image"),
        "init 不得再把 PT_INTERP 改写成 elfld"
    );
}

#[test]
fn exec_reads_interpreter_and_calls_dual_loader() {
    assert!(
        EXEC.contains("crate::privileged::proc::elf::interp_path(&image)"),
        "execve 必须在主映像上读取 PT_INTERP"
    );
    assert!(
        EXEC.contains("read_file_kernel(path, current_pwm, ELF_MAX_SIZE)"),
        "解释器必须经 VFS 读入，而不是内核内嵌"
    );
    assert!(
        EXEC.contains("proc_exec_replace_with_interp"),
        "execve 必须把双映像交给新入口"
    );
    assert!(
        INIT.contains("crate::privileged::proc::elf::interp_path(&elf)")
            && INIT.contains("user_proc_load_elf_from_memory_with_interp"),
        "init 必须走同一套双映像装载"
    );
}

#[test]
fn dual_loader_selects_interp_entry_and_separate_bias() {
    assert!(
        USER_PROC.contains("pub fn load_elf_from_memory_with_interp(")
            && USER_PROC.contains("Self::load_elf_images("),
        "双映像核心入口必须存在"
    );
    assert!(
        USER_PROC.contains("let entry = interp_meta")
            && USER_PROC.contains("|(v, _)| v.entry + interp_bias"),
        "有解释器时入口必须归解释器"
    );
    assert!(
        USER_PROC.contains("if interp_end > mmap_base")
            && USER_PROC.contains("mmap_base = interp_end.saturating_add(PAGE_SIZE)"),
        "解释器占用 mmap 基址后，后续 mmap 区必须上抬"
    );
    assert!(
        USER_PROC.contains("mm.start_brk.store(") && USER_PROC.contains("usize_from_u64(main_end)"),
        "brk 必须只跟主映像，不得被解释器抬到 mmap 区"
    );
}

#[test]
fn d3_initial_stack_carries_auxv() {
    assert!(
        ELF_MOD.contains("pub fn phdr_addr(elf_data: &[u8], load_bias: u64) -> Option<u64>"),
        "AT_PHDR 必须由包含 e_phoff 的 PT_LOAD 反推"
    );
    assert!(
        USER_PROC.contains("setup_user_stack_in_table(")
            && USER_PROC.contains("auxv: &[(u64, u64)]")
            && USER_PROC.contains("AT_PHDR")
            && USER_PROC.contains("AT_RANDOM")
            && USER_PROC.contains("AT_EXECFN"),
        "初始栈必须写 argc/argv/envp/auxv 四段"
    );
    assert!(
        USER_PROC.contains("super::elf::phdr_addr(main_elf")
            && USER_PROC.contains("store_elf_meta("),
        "装载期必须把 AT_PHDR/AT_ENTRY/AT_BASE 存到进程"
    );
    assert!(
        PROC_OPS.contains("USER_PROC_MANAGER.auxv_for_pid(new_pid_u32)")
            && INIT.contains("user_proc_setup_empty_argv"),
        "exec 与 init 都必须建立带 auxv 的栈"
    );
}

#[test]
fn ffi_surfaces_keep_static_wrapper() {
    assert!(
        PROC_API.contains("user_proc_load_elf_from_memory_with_interp")
            && PROC_OPS.contains("proc_exec_replace_with_interp"),
        "双映像必须有独立 FFI 入口"
    );
    assert!(
        PROC_OPS.contains("proc_exec_replace_with_interp(\n        elf_addr, elf_size, 0, 0"),
        "旧 proc_exec_replace 必须保留为静态包装，避免调用方断裂"
    );
}
