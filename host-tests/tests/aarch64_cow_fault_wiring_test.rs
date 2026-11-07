// SPDX-License-Identifier: MPL-2.0
// aarch64 缺页 / COW 恢复路径接线回归测试 (#108 丙).
//
// 背景:
//   - aarch64 侧此前**没有**缺页恢复路径: EL0 非 SVC 同步异常一律"打印现场 + 终止/停机",
//     于是 `mark_cow_readonly` 的 aarch64 分支只能恒返回原值 (fork 不置只读), 归档
//     docs/plan/archive/cr3-lifetime-ownership.md 裁定 7 据此定形.
//   - 本批补齐整链: `ESR_EL1` 解码 → 共用缺页入口 `handle_user_page_fault` → 页表/帧计数
//     → 返回原 `ELR` 重试. 补齐后 fork 两侧都置只读, 裁定 7 被回判.
//   - 恢复面有**两条**: EL0 来源走共用缺页入口; 同 EL (核态 uaccess 写) 走窄支路
//     `break_cow_for_kernel_uaccess` —— 后者是 AP[2] 挡 EL1 写的连带后果, 缺它则 fork 之后
//     任何核态写用户页都成了内核故障 (真机读数 `ESR=0x9600004F FAR=<用户地址> ELR=<内核 .text>`).
//
// 为何要静态锁 (host 跑不了 aarch64 机器码):
//   1. **权限位极性** —— aarch64 的 AP[2](bit7) 与 x86 的 WRITABLE(bit1) 方向相反:
//      ARM 是**置位**才只读. 写成"清 bit7"能编译、能在 host 全绿, 只在真机上表现为
//      "COW 永不触发 ⇒ 父子写同一物理页". 位形断言在 `privileged/tests/test_mm.rs`
//      (on-device, 两架构各自判), 本文件锁源码侧的极性写法不被改回.
//   2. **恢复分支的接线次序** —— 射程判定必须早于裸 UART 现场打印 (否则每次正常缺页都
//      刷控制台), 也必须早于 `process_exit`; 且只有 `PfResult::Fixed` 可以返回重试.
//   3. **真用户页表根** —— 缺页路径必须按本核 KPTI 槽的 `user_ttbr0` 操作; 按当前
//      TTBR0 (此刻已是本进程 EL1 视图根) 建映射会与用户表分叉, 表现为重试再故障.
//   4. **重试无需汇编改动** —— `handle_el0_sync` 的 `bl sync_exception_handler` 之后
//      必须仍是 `b el0_return` (以帧内未改动的 ELR eret); 该出口若被改成直接 eret/停机,
//      "返回即重试"的契约失效.
//   5. **`EC` 极性** —— ARMv8 把"来自低异常级"编成**偶数** `EC`、"异常级不变"编成**奇数**
//      (数据 `0x24`/`0x25`, 取指 `0x20`/`0x21`), 与"低位=同 EL"的位序直觉相反. 写反能让
//      host 全绿, 真机上表现为 `access.user` 整体倒置 —— EL0 侧的按需分页/栈增长腿与核态
//      uaccess 支路同时静默失效. 故锁常量表与 `EC` 前置判.
//   6. **核态 uaccess 支路的准入面** —— ARM 的 AP[2] 对 EL1 同样生效 (页表里没有"EL0 只读
//      / EL1 可写"这一组合), 故 fork 置只读后**内核**写用户页也是同 EL 权限故障. 该支路
//      只允许"权限故障 + 写 + 同 EL + FAR 落用户半区"这一解码形态, **且**页侧三条 (用户
//      leaf + 描述符只读 + 本帧带 COW 待破标) 同时成立, 破 COW 必须走与 EL0 同一入口
//      `cow_handle_fault` —— 走通用 VMA 处置 (`handle_vma_fault_with_mm`) 会被"可写 VMA
//      的权限故障"判成普通匿名页首访, 结果是**把已有页清零**.
//   7. **COW 待破标的载体与生命周期** —— 判据 3 的证据落在 PMM 的第 6 块簿记区
//      (`FRAME_COW`, 每帧 1 字节). 区未被位图预留 ⇒ 元数据踩元数据; 清位点缺任一处 ⇒
//      帧永久"待破", 后续持有者在真·只读映射上的写会被本支路升成可写 (违 I2/I4).
//      分配侧与归还侧双向都清 + fork 置位点唯一 + 就地恢复清位, 四点同锁.
//
// 运行期判据 (真链路真跑通) 由 `scripts/aarch64_cow_boot_test.sh` 承担, 本文件不重复.

use std::fs;

const A64_EXCEPTION: &str = "../src/kernel/privileged/arch/aarch64/exception.rs";
const A64_KPTI: &str = "../src/kernel/privileged/mm/kpti_aarch64.rs";
const COW: &str = "../src/kernel/privileged/mm/cow.rs";
const MM_MOD: &str = "../src/kernel/privileged/mm/mod.rs";
const PAGE_FAULT: &str = "../src/kernel/privileged/mm/page_fault.rs";
const PMM: &str = "../src/kernel/privileged/mm/pmm.rs";

fn read(p: &str) -> String {
    fs::read_to_string(p).unwrap_or_else(|e| panic!("read {p}: {e}"))
}

/// 去掉整行注释 (含 `///` 文档注释与本批新增的大段中文说明), 避免注释文本被误判为代码.
fn code_only(src: &str) -> String {
    src.lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// 压缩连续空白为单空格, 使断言不受换行/列对齐影响.
fn norm(src: &str) -> String {
    src.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// 取 `signature` 对应函数的源码窗口 (从签名起到首个行首 `}` 为止).
fn fn_body(src: &str, signature: &str) -> String {
    let start = src
        .find(signature)
        .unwrap_or_else(|| panic!("未找到函数签名 {signature:?}"));
    let rest = &src[start..];
    let end = rest.find("\n}").map_or(rest.len(), |i| i + 2);
    rest[..end].to_string()
}

/// `impl` 块内方法版: 结束判据取 rustfmt 的 4 空格收口 (`\n    }`).
///
/// 必需 —— `pmm.rs` 的被锁方法都在 `impl PhysicalMemoryManager` 里, 自身收口带缩进,
/// 用 [`fn_body`] 的顶格 `}` 判据会把窗口一路开到 `impl` 结束, 把相邻方法的文本也算进来,
/// "某清位点在 `do_alloc` 里"这类断言就此失去判别力.
fn impl_fn_body(src: &str, signature: &str) -> String {
    let start = src
        .find(signature)
        .unwrap_or_else(|| panic!("未找到方法签名 {signature:?}"));
    let rest = &src[start..];
    let end = rest.find("\n    }").map_or(rest.len(), |i| i + 6);
    rest[..end].to_string()
}

/// 断言 `a` 在 `b` 之前出现 (两者都必须存在).
fn assert_before(hay: &str, a: &str, b: &str) {
    let ia = hay.find(a).unwrap_or_else(|| panic!("{a:?} 未找到"));
    let ib = hay.find(b).unwrap_or_else(|| panic!("{b:?} 未找到"));
    assert!(ia < ib, "{a:?} 必须出现在 {b:?} 之前");
}

/// 1. 恢复分支已接线, 且次序为"射程判定 → 打印 → 终止".
#[test]
fn test_a64_recovery_gate_wired_before_print() {
    let src = code_only(&read(A64_EXCEPTION));
    let body = fn_body(&src, "pub extern \"C\" fn sync_exception_handler");
    let flat = norm(&body);

    // EL0 来源判据 + 故障类判据 (两条都在同一个 if 上).
    assert!(
        flat.contains("frame.spsr.trailing_zeros() >= 4"),
        "恢复射程必须只对 EL0 来源开放 (内核态同步异常须停机, 不得当作进程缺页)"
    );
    assert!(
        flat.contains("A64FaultClass::from_esr(esr).is_demand_pageable()"),
        "恢复射程必须按 ESR 故障类收窄 (只有翻译/权限故障可恢复)"
    );
    // 共用缺页入口 + ESR 适配器.
    assert!(
        flat.contains("PageFaultInfo::from_esr(esr, far)"),
        "必须经 ESR 适配器产出与 x86_64 同一结构的缺页信息"
    );
    assert!(
        flat.contains("handle_user_page_fault(pf_info)"),
        "必须复用两架构共用的缺页入口 (而非在 aarch64 侧另写一套处置)"
    );
    // 只有 Fixed 才返回重试; 提前返回只允许两处 (EL0 缺页重试 + 核态破 COW 重试).
    assert!(
        flat.contains("PfResult::Fixed { return; }"),
        "只有 PfResult::Fixed 可以返回 EL0 重试 (含 UffdWait 在内的其余结果须落到终止路径)"
    );
    assert_eq!(
        body.matches("return;").count(),
        2,
        "sync_exception_handler 只允许两处提前返回 (EL0 缺页重试 / 核态 uaccess 破 COW 重试); \
         新增即需重新论证 —— 每一处 `return` 都意味着\"以未改动的 ELR 重试触发异常的指令\""
    );
    // 次序: 两条恢复判定都早于裸 UART 打印, 打印早于终止.
    assert_before(
        &body,
        "is_demand_pageable()",
        "break_cow_for_kernel_uaccess(esr, far)",
    );
    assert_before(
        &body,
        "break_cow_for_kernel_uaccess(esr, far)",
        "putc(b'S')",
    );
    assert_before(&body, "putc(b'S')", "process_exit");
}

/// 2. 重试出口仍复用 `el0_return` (以帧内未改动的 ELR eret), 汇编侧无需也不应改动.
#[test]
fn test_a64_el0_sync_falls_through_to_el0_return() {
    let src = read(A64_EXCEPTION);
    let start = src
        .find("handle_el0_sync:")
        .expect("未找到 handle_el0_sync 标签");
    let end = src[start..]
        .find("handle_svc:")
        .unwrap_or_else(|| panic!("未找到 handle_svc 标签"));
    let win = norm(&src[start..start + end]);
    // SVC 分派必须先于非 SVC 兜底 (syscall 路径不得被当作假故障).
    // 汇编行尾有注释, 故只判次序与各自存在, 不判连续行.
    assert_before(&win, "cmp x0, #0x15", "bl sync_exception_handler");
    assert!(
        win.contains("beq handle_svc"),
        "SVC 必须独立分派到 handle_svc"
    );
    assert!(
        win.contains("mov x0, sp bl sync_exception_handler b el0_return"),
        "EL0 非 SVC 同步异常必须调 handler 后落到 el0_return —— \
         缺页恢复就是“从 Rust 返回”, 该出口被改写即切断重试链"
    );
}

/// 3. `mark_cow_readonly` 的 aarch64 极性: 置 AP[2] 且保持 AP[1], 不是清位.
#[test]
fn test_cow_readonly_bit_polarity() {
    let src = code_only(&read(COW));
    let flat = norm(&fn_body(&src, "fn mark_cow_readonly(entry: u64) -> u64"));

    assert!(
        flat.contains("entry | (1 << 7)"),
        "aarch64 置只读必须**置** AP[2](bit7): ARM 的 AP[2:1]=0b11 才是 EL0 只读, \
         沿用 x86 的“清可写位”直觉会得到 0b01 (EL0 读写) ⇒ fork 后 COW 永不触发"
    );
    assert!(
        !flat.contains("entry & !(1 << 7)"),
        "不得以清 bit7 实现 aarch64 只读 (极性相反)"
    );
    assert!(
        flat.contains("is_page_leaf && ap1_set && ap2_clear"),
        "只读化必须只对“用户可写的 4KB 页 leaf”生效: AP[1](bit6) 既是 EL0 可读性条件, \
         也是 is_user_leaf 判据位, 清掉它会令 fork 的 frame_inc 与拆除侧的 frame_dec 失配"
    );
    // x86_64 侧方向相反 (清 WRITABLE), 两架构不得互相"对齐"成同一写法.
    assert!(
        flat.contains("entry & !0b10"),
        "x86_64 侧只读化仍是清 bit1 (WRITABLE)"
    );
}

/// 4. 缺页路径的用户页表根取自本核 KPTI 槽, 不是当前 TTBR0.
#[test]
fn test_user_root_comes_from_kpti_slot() {
    let mm = code_only(&read(MM_MOD));
    let body = norm(&fn_body(&mm, "pub fn read_user_cr3_asm() -> u64"));
    assert!(
        body.contains("kpti_get_user_ttbr0()"),
        "aarch64 必须读本核 KPTI 槽的 user_ttbr0 (异常入口已把 TTBR0 精化为本进程 EL1 视图根)"
    );
    assert_before(&body, "kpti_get_user_ttbr0()", "vmm::get_current_pml4()");

    let kpti = code_only(&read(A64_KPTI));
    let getter = norm(&fn_body(&kpti, "pub fn kpti_get_user_ttbr0() -> u64"));
    assert!(
        getter.contains("user_ttbr0.load(Ordering::Acquire)"),
        "getter 必须与 setter (kpti_set_user_ttbr0) 共用同一每核槽字段"
    );

    // 缺页入口必须经该单点取根, 不得就地读硬件当前表.
    let pf = code_only(&read(PAGE_FAULT));
    let entry = norm(&fn_body(
        &pf,
        "pub fn handle_user_page_fault(info: PageFaultInfo) -> PfResult",
    ));
    assert!(
        entry.contains("read_user_cr3_asm()"),
        "缺页入口的用户页表根必须来自 read_user_cr3_asm 单点"
    );
}

/// 5. ESR 适配器的射程与 present 口径不被悄悄放宽.
#[test]
fn test_esr_adapter_scope_locked() {
    let src = code_only(&read(PAGE_FAULT));
    let flat = norm(&src);

    assert!(
        flat.contains("pub fn from_esr(esr: u64, fault_addr: u64) -> Self"),
        "ESR 适配器必须与 x86 的 from_error_code 一样产出 PageFaultInfo (同一结构体, 共用处置)"
    );
    assert!(
        flat.contains("matches!(self, Self::Translation | Self::Permission)"),
        "恢复射程只能是 翻译 + 权限 两类; AF/外部同步/对齐/取指权限须留在射程外"
    );
    assert!(
        flat.contains("present: !matches!(class, A64FaultClass::Translation)"),
        "present 只由故障类决定: 翻译故障 = 描述符不存在, 权限故障 = 存在但不可写 (COW 判据)"
    );
    assert!(
        flat.contains("reserved: false"),
        "aarch64 stage-1 无保留位故障类, reserved 须恒 false (SignalBus 分支在本架构不可达)"
    );
}

/// 6. `EC` 极性表: 低 EL = 偶数编码, 同 EL = 奇数编码; 归类必须先按 `EC` 分档.
///
/// 真机读数 (aarch64 QEMU, init 的两条腿): EL0 取内核高半区 → `ESR=0x92000006`
/// (`EC=0x24`); 内核写用户 COW 页 → `ESR=0x9600004F` (`EC=0x25`). 常量表写反在 host
/// 全绿, 真机表现为 `access.user` 整体倒置 —— EL0 的按需分页/栈增长腿与核态 uaccess
/// 支路同时静默失效, 且两条腿的射程判据互换 (内核故障被当进程缺页处理).
#[test]
fn test_a64_esr_ec_polarity_locked() {
    let flat = norm(&code_only(&read(PAGE_FAULT)));

    for needle in [
        "const EC_INSTR_ABORT_LOWER_EL: u64 = 0x20;",
        "const EC_INSTR_ABORT_SAME_EL: u64 = 0x21;",
        "const EC_DATA_ABORT_LOWER_EL: u64 = 0x24;",
        "const EC_DATA_ABORT_SAME_EL: u64 = 0x25;",
    ] {
        assert!(
            flat.contains(needle),
            "`EC` 常量表被改动 ({needle:?}) —— ARMv8 的\"低 EL=偶数 / 同 EL=奇数\"由真机读数钉住"
        );
    }
    // 倒置形态 (沿用"低位=同 EL"的位序直觉) 不得回来.
    assert!(
        !flat.contains("const EC_DATA_ABORT_LOWER_EL: u64 = 0x25;"),
        "数据异常的低 EL 编码是 0x24 不是 0x25"
    );

    // `user` 位只由 `EC` 的"低 EL"一档决定 (取指+数据各一条), 与故障类无关.
    assert!(
        flat.contains("user: matches!(ec, EC_INSTR_ABORT_LOWER_EL | EC_DATA_ABORT_LOWER_EL)"),
        "access.user 必须取\"来自低异常级\"的两条 EC (0x20/0x24)"
    );
    // 归类必须被 `EC` 前置判挡住取指异常: IFSC 的翻译段 (0b0001LL) 与 DFSC 同形.
    assert!(
        flat.contains("!matches!(ec, EC_DATA_ABORT_LOWER_EL | EC_DATA_ABORT_SAME_EL)"),
        "A64FaultClass::from_esr 必须有 EC 前置判, 否则取指翻译故障被误判为可恢复"
    );
}

/// 7. 核态 uaccess 破 COW 支路的准入面与处置入口不被放宽.
///
/// 该支路是"AP[2] 同时挡 EL1 写"的连带后果 (裁定乙). 它一旦放宽, 代价是**把内核故障当
/// 缺页掩盖**: 翻译故障 (真·未映射)、已可写页上的写、以及真·只读映射 (内核映射的只读页)
/// 若被吞掉, 要么重试再故障成缺页风暴, 要么把只读页升成可写 (违 I2/I4).
/// 故"解码四条 + 页侧三条"与"只走 `cow_handle_fault` 单点"全部锁死.
///
/// 页侧第三条取证凭据是 **per-frame COW 待破标** (`pmm::frame_cow_test`, 裁定乙). 本批
/// 第一版用帧计数 (`frame_ref_count < 2` 即拒), 真机连停两次才归因到两个缺陷: 一是
/// `CURRENT_MM` 只在 execve 写一次 (以 VMA 为准会把合法页判成"不属任何 VMA"), 二是计数
/// 本身证不了 COW —— 子进程退出后父侧 leaf 仍是 COW 待破而计数已回到 1. 倒退回填即红线.
#[test]
fn test_kernel_uaccess_cow_branch_scope_locked() {
    let src = code_only(&read(PAGE_FAULT));

    // 解码面准入: 同 EL + 写 + 权限故障 + FAR 落用户半区, 四条缺一不成立.
    let cand = norm(&fn_body(
        &src,
        "fn kernel_uaccess_cow_candidate(esr: u64, fault_addr: u64) -> bool",
    ));
    for needle in [
        "A64FaultClass::Permission",
        "info.access.write",
        "!info.access.user",
        "fault_addr < crate::privileged::constants::limits::USER_ADDR_MAX",
    ] {
        assert!(
            cand.contains(needle),
            "解码准入判据 {needle:?} 不得丢: 少一条即把某类真·内核故障当成 COW 吞掉"
        );
    }

    // 页侧准入: 表根取当前 TTBR0 (即翻译这次核态访问的那张表); 描述符须是用户 leaf、
    // 确实只读、且带 COW 待破标; 破 COW 走与 EL0 同一入口.
    let body = norm(&fn_body(
        &src,
        "pub fn break_cow_for_kernel_uaccess(esr: u64, fault_addr: u64) -> bool",
    ));
    assert!(
        body.contains("let user_cr3 = vmm::get_current_pml4();"),
        "同 EL 访问由 TTBR0 当前表翻译 ⇒ 必须改那张表; 用 read_user_cr3_asm() 的 KPTI 槽根会改到另一张表"
    );
    assert!(
        body.contains("super::is_user_leaf(pte)"),
        "判据 1 不得丢: 本支路只管用户半区的 4KB leaf; supervisor 页与大页/表项描述符 \
         落进来越权改写内核映射 (违 I2)"
    );
    assert!(
        body.contains("super::Vmm::translate_in_pml4(user_cr3, VirtAddr(aligned))"),
        "必须先看叶子描述符 (真机读数只能证明\"权限不符\", 证不了\"这张表里它确实只读\")"
    );
    assert!(
        body.contains("if leaf.writable { return false; }"),
        "已可写的页上没有 COW 可破 —— 该形态是别的病灶, 不得由本支路代答"
    );
    assert!(
        body.contains(
            "pmm::get_pmm().frame_cow_test(PhysAddr(leaf.phys.as_u64() & 0x000F_FFFF_FFFF_F000))"
        ),
        "判据 3 必须是 per-frame COW 待破标 (裁定乙): 描述符分不开 COW 只读与真·只读映射 \
         (AP_BOTH_RO 逐位同形), 帧计数也分不开 (子进程退出后计数回到 1)"
    );
    assert!(
        !body.contains("frame_ref_count"),
        "不得回到\"帧计数 >= 2 才算 COW\"的判据 (本批第一版形态): 计数证不了 COW, \
         且多页块计数口径改动后它既可能误收真·只读映射也可能漏掉合法 COW 页"
    );
    assert!(
        body.contains("super::cow::cow_handle_fault(user_cr3, fault_addr)"),
        "破 COW 必须复用 EL0 侧同一入口 (帧记账不分叉)"
    );
    assert!(
        !body.contains("handle_vma_fault_with_mm"),
        "不得走通用 VMA 缺页处置: 可写 VMA 上的权限故障在那里被判成\"普通匿名页首访\"⇒ 清零已有页"
    );
    assert!(
        !body.contains("read_user_cr3_asm()"),
        "本支路的表根口径与 EL0 侧不同 (见上), 两者不得互相\"对齐\""
    );
    assert!(
        !body.contains("get_current_mm()"),
        "不得回到 VMA/CURRENT_MM 准入: 该全局只在 execve 写一次, 既不 per-CPU 也不随 fork 更新 \
         ⇒ 真机把合法 COW 页判成射程外 (本批第一版的两次停机)"
    );
}

/// 8. per-frame COW 待破标 (§裁定乙) 的元数据区与生命周期闭合点.
///
/// 标记位若只是散落在页表描述符里的一个位, 就没有"分辨 COW 与真·只读"的凭据; 若区未
/// 被位图预留, 它自身会被 buddy 当空闲页发放 (元数据踩元数据); 若清位点缺失, 一个帧
///  once 被标过 COW 就永久"待破", 后续任何持有者在真·只读映射上写都会被本支路升成可写
/// (违 I2/I4). 故四处闭合点全部锁死: 分配清位 / 归还清位 / fork 置位 / 就地恢复清位.
#[test]
fn test_frame_cow_mark_region_and_lifecycle_locked() {
    let pmm = code_only(&read(PMM));
    let flat = norm(&pmm);

    // 区的建立: 尺寸由 total_pages 推导 (非手写常量), 载体预置, 位图预留, 有启动日志.
    for needle in [
        "let cow_bytes = total_pages;",
        "store.setup_cow(layout.cow_phys, layout.cow_bytes)",
        "phys_to_page(layout.cow_phys)",
        "[PMM] FRAME_COW",
    ] {
        assert!(
            flat.contains(needle),
            "FRAME_COW 区形状被改动 ({needle:?}) —— 未预留即元数据踩元数据, 尺寸不推导即漂移"
        );
    }

    // 生命周期闭合: 置位只在 fork (cow.rs), 清位在就地恢复 + do_alloc + do_free.
    let cow = code_only(&read(COW));
    assert!(
        norm(&cow).contains("pmm.frame_cow_mark(leaf_phys)"),
        "fork 必须在**真正置只读成功**的那一刻打标记; 无条件打则把未写保护的页也算成待破"
    );
    let reuse = norm(&fn_body(
        &cow,
        "fn cow_handle_fault_locked(pml4: u64, fault_addr: u64) -> Option<u64>",
    ));
    assert!(
        reuse.contains("pmm_inst.frame_cow_clear(PhysAddr(old_frame))"),
        "就地恢复可写的那一支必须清位, 否则该帧永久\"待破\""
    );
    let do_alloc = norm(&impl_fn_body(
        &pmm,
        "fn do_alloc(&self, order: u8) -> Option<PhysAddr>",
    ));
    let do_free = norm(&impl_fn_body(
        &pmm,
        "fn do_free(&self, addr: PhysAddr, order: u8)",
    ));
    assert!(
        do_alloc.contains("self.frame_cow_write(bit, 0)"),
        "再分配必须清位: 上一手的 COW 标不得跟着帧进入新持有者"
    );
    assert!(
        do_free.contains("self.frame_cow_write("),
        "归还必须清位 (与 do_alloc 同口径, 双向都清才不会残留)"
    );

    // 越界 fail-closed: MMIO 一类非 PMM 地址不得被标记 (标记区与计数区同值域).
    let mark = norm(&impl_fn_body(
        &pmm,
        "pub fn frame_cow_mark(&self, addr: PhysAddr) -> bool",
    ));
    assert!(
        mark.contains("self.frame_counts_pfn(addr)"),
        "frame_cow_mark 必须复用计数区的值域校验 (单一边界口径)"
    );
    assert!(
        mark.contains("None => false"),
        "越界地址必须 fail-closed 返回 false (非 PMM 映射不参与 COW)"
    );
}
