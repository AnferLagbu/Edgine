//! MM 回归锁: 页表帧 / 物理帧所有权元数据 — 异常归还必须可点名归因
//!
//! 工程登记见 `docs/plan/net-e2e-tcp-echo-flakiness.md` 的抖动取证线: x86_64
//! 端到端存在一类"init 在 fork / `destroy_page_table` 边界停住"的失败形态, 现场
//! 伴生 `[PMM] Warn: double free at pfn N` 落在页表帧分配带内. 旧告警只报"哪一帧
//! 被二次释放", 不报"这帧原本属于谁、被谁退役、现在又是谁在还" —— 三类信息各自
//! 散在不同子系统, 单凭一帧地址无法点名争抢双方. 本机制把这三者固化成 PMM 的第
//! 5 块簿记区, 属 I2 (内核内存不可被非法访问) 与 I6 (DMA/重分配不可指向在用帧)
//! 的运行时自检面, **长期常驻** (非取证脚手架).
//!
//! # 被锁死的六条不变式
//!
//! 1. **FRAME_OWNER 是 init_bitmap 的第 5 区并被位图预留**: 独立数组按 pfn 索引,
//!    每帧 16 字节 (分配点 + 最近归还点). 不复用 `buddy_meta` / `FREE_LINKS` ——
//!    那两块的字节已被 buddy 阶数与空闲链关系占用; 未预留则所有权槽自身会被当
//!    空闲页分配出去, 变成"元数据踩元数据".
//! 2. **归因写入点唯一且粒度正确**: 分配点只在 `alloc_page` / `alloc_pages` 的
//!    成功分支记 (块记头页, 与计数面同粒度); 退役点必须在**入延迟释放链**的时刻记.
//!    只在 `free_page` 记等于没记 —— 经链归还的帧其 `free_page` 调用点恒为
//!    `deferred_free::free_chain`, 归因价值为零.
//! 3. **`#[track_caller]` 穿透到底**: 入口 (`alloc_page` / `free_page` /
//!    `do_free`) 与 api 层的 `*_phys` 包装都必须带该属性, 否则 `Location::caller()`
//!    落到包装层自身, 三元组全变 pmm.rs / api.rs 行号, 机制失去意义.
//! 4. **异常归还早退不入链**: `do_free` 见位图已清即告警返回, 不得把已在空闲链
//!    中的帧再挂一次 (二次入链会让链表成环, 那才是真正不可恢复的腐败).
//! 5. **观测面不得打挂控制流**: 退役归因入口用软取单例 (`GLOBAL_PMM.get()`),
//!    与 `free_chain` 的"空链不取 PMM"同一条约束 —— host 测试仅测链逻辑时 PMM
//!    单例并未建立; 载体未就绪 / pfn 越界一律静默跳过.
//! 6. **双载体同构**: trait 的每个所有权方法在 `RawMetaStore` (生产) 与
//!    `VecMetaStore` (host) 各有一份实现. 少一份则要么编译不过, 要么 host 侧
//!    覆盖不到该语义 (H-04 方案 B 的核心承诺: 算法只有一份代码).
//!
//! # 断言方式
//!
//! 源码文本契约 (剥注释 + 去空白), 只锁语义形状, 不受 rustfmt 换行影响; 需要
//! 片段位置的断言用未去空白的生产文本取函数体. 判定逻辑收敛成 `has_*` /
//! `count_*` 谓词, 由 [`test_negative_control_predicates_reject_regressions`]
//! 用合成回退样本反向验证 —— 谓词对真实文件返 `true` 才有意义, 前提是它确实
//! 能在回归发生时返 `false`.

use std::fs;

const PMM_RS: &str = "../src/kernel/privileged/mm/pmm.rs";
const DEFERRED_FREE_RS: &str = "../src/kernel/privileged/mm/deferred_free.rs";
const MM_MOD_RS: &str = "../src/kernel/privileged/mm/mod.rs";
const API_RS: &str = "../src/kernel/privileged/mm/api.rs";

// ============================================================================
// 文本读取与规范化
// ============================================================================

fn read(path: &str) -> String {
    fs::read_to_string(path).unwrap_or_else(|e| panic!("读取 {path} 失败: {e}"))
}

/// 生产代码文本: 剥掉注释行, 并在第一个 `#[cfg(test)]` 处截断.
///
/// 本机制的动机注释逐字提到 `deferred_free::free_chain` / "二次释放" 等被扫描
/// 形态, 不剥注释则锁的对象会变成注释.
fn prod_code(src: &str) -> String {
    src.lines()
        .map(str::trim)
        .filter(|l| !l.starts_with("//") && !l.starts_with("/*") && !l.starts_with('*'))
        .take_while(|l| !l.starts_with("#[cfg(test)]"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// 去掉全部空白, 使形状断言不受换行与缩排影响.
fn squash(code: &str) -> String {
    code.chars().filter(|c| !c.is_whitespace()).collect()
}

fn prod_squashed(path: &str) -> String {
    squash(&prod_code(&read(path)))
}

/// 截取 `src` 中首个含 `begin` 的行到该函数 / 结构体的结束行.
///
/// 结束判据取 rustfmt 的硬约束: 顶层项的右括号独占一行且位于第 0 列
/// (`}`); 嵌套块的同名行始终带缩进. 不用括号计数 —— 被扫函数内的 klog 格式
/// 串带 `{}`, 计数会被字符串里的括号带偏.
fn fn_body(src: &str, begin: &str) -> String {
    let mut out = String::new();
    let mut started = false;
    for line in src.lines() {
        if !started {
            if line.contains(begin) {
                started = true;
            } else {
                continue;
            }
        }
        out.push_str(line);
        out.push('\n');
        if line == "}" {
            break;
        }
    }
    assert!(started, "未找到起点: {begin}");
    out
}

/// 从函数体里截出含 `marker` 的那条语句块 (到其自身右括号止).
///
/// 结束判据同样是 rustfmt 约束: 块末 `}` 与块首 `if` 同缩进.
fn block_from(body: &str, marker: &str) -> String {
    let lines: Vec<&str> = body.lines().collect();
    let at = lines
        .iter()
        .position(|l| l.contains(marker))
        .unwrap_or_else(|| panic!("未找到块起点: {marker}"));
    let indent = lines[at].len() - lines[at].trim_start().len();
    let close = " ".repeat(indent);
    let mut out = String::new();
    for line in &lines[at..] {
        out.push_str(line);
        out.push('\n');
        if *line == format!("{close}}}") {
            break;
        }
    }
    out
}

/// `sig` 定义前紧邻的属性行中是否含 `attr` (向前看 6 行, 跨过派生与期望属性).
fn attr_before_sig(src: &str, sig: &str, attr: &str) -> bool {
    let prod = prod_code(src);
    let at = prod
        .find(sig)
        .unwrap_or_else(|| panic!("未找到签名: {sig}"));
    prod[..at].lines().rev().take(6).any(|l| l.trim() == attr)
}

/// `needle` 在 `hay` 中出现的次数.
fn count(hay: &str, needle: &str) -> usize {
    hay.matches(needle).count()
}

// ============================================================================
// 谓词 (可被负控制反向验证)
// ============================================================================

/// 不变式 1: init_bitmap 里 FRAME_OWNER 区完整建立 (尺寸推导 + 载体预置 + 位图预留).
fn has_frame_owner_region(init_bitmap_squashed: &str) -> bool {
    let need_all = [
        "total_pages*core::mem::size_of::<OwnerIndex>()",
        "store.setup_owner(layout.owner_phys,layout.owner_bytes)",
        "letowner_start_page=crate::privileged::lib::usize_from_u64(phys_to_page(layout.owner_phys))",
        "self.set_bit(i)",
        "[PMM]FRAME_OWNER",
    ];
    need_all.iter().all(|n| init_bitmap_squashed.contains(n))
}

/// 不变式 1 反面: FRAME_COUNTS 与 FRAME_OWNER 尺寸不同源 (手写常量必漂移).
fn frame_owner_size_is_derived(init_bitmap_squashed: &str) -> bool {
    init_bitmap_squashed
        .contains("owner_bytes.div_ceil(crate::privileged::lib::usize_from_u64(PAGE_SIZE))")
        && init_bitmap_squashed.contains("owner_phys+owner_pages*PAGE_SIZE+PAGE_SIZE")
}

/// 不变式 6: trait 声明 + 两份实现, 每个所有权方法恰出现 3 次.
fn owner_methods_are_symmetric(pmm_squashed: &str) -> bool {
    [
        "setup_owner",
        "owner_read",
        "owner_mark_alloc",
        "owner_mark_release",
    ]
    .iter()
    .all(|m| count(pmm_squashed, &format!("fn{m}(")) == 3)
}

/// 不变式 2: 分配点归因只在两个分配入口的成功分支 (块记头页).
fn count_alloc_attribution_sites(pmm_prod: &str) -> usize {
    count(pmm_prod, "self.note_frame_alloc(")
}

/// 不变式 2: 退役点归因必须挂在入链时刻 (defer_free), 不是只在 free_page.
fn has_retire_attribution_at_enqueue(defer_free_body: &str) -> bool {
    defer_free_body.contains("note_frame_retired(") && defer_free_body.contains("BATCH_HEAD.store(")
}

/// 不变式 3: `sig` 之前紧邻 `#[track_caller]`.
fn has_track_caller(src: &str, sig: &str) -> bool {
    attr_before_sig(src, sig, "#[track_caller]")
}

/// 不变式 4: 二次释放分支只告警并早退, 不触碰位图 / 空闲链.
fn double_free_bails_out(abort_branch: &str) -> bool {
    let mentions_triple = ["alloc=", "first_retire=", "this_free="]
        .iter()
        .all(|n| abort_branch.contains(n));
    mentions_triple
        && abort_branch.contains("return;")
        && !abort_branch.contains("clear_bit(")
        && !abort_branch.contains("buddy_list_push(")
        && !abort_branch.contains("stats_free(")
}

/// 从 `do_free` 体内截出"位图已清"分支 (到该分支自身的右括号为止).
fn double_free_branch(do_free_body: &str) -> String {
    block_from(do_free_body, "if !self.test_bit(")
}

/// 不变式 5: 观测入口软取单例, 不含 panic 路径.
fn observer_never_panics(body: &str) -> bool {
    body.contains("GLOBAL_PMM.get()")
        && !body.contains("get_or_panic")
        && !body.contains(".expect(")
}

/// 不变式 5: 记录函数自持 PMM 锁 (trait 契约: 载体读写须在锁下).
fn retire_attribution_holds_pmm_lock(body: &str) -> bool {
    body.contains("self.acquire_lock()") && body.contains("self.release_lock(")
}

/// 越界 fail-closed: 退役归因先做 pfn 值域校验 (非 PMM 地址不写元数据).
fn retire_attribution_bounds_checked(body: &str) -> bool {
    let squashed = squash(body);
    squashed.contains("letSome(pfn)=self.frame_counts_pfn(addr)") && squashed.contains("return")
}

// ============================================================================
// 不变式 1: FRAME_OWNER 区存在且被预留
// ============================================================================

#[test]
fn frame_owner_region_is_built_and_reserved() {
    let pmm = read(PMM_RS);
    // init_bitmap 已按阶段拆为编排体 + 三个 helper (布局推导 / 载体预置 / 位图预留),
    // 不变式跨这三段成立, 故拼合后判定.
    let built = squash(&prod_code(&format!(
        "{}{}{}",
        fn_body(&pmm, "fn plan_meta_layout"),
        fn_body(&pmm, "fn install_meta_store"),
        fn_body(&pmm, "fn mark_reserved_pages"),
    )));
    assert!(
        has_frame_owner_region(&built),
        "init_bitmap 未完整建立 FRAME_OWNER 区 (尺寸推导 / setup_owner / 位图预留 / 日志)"
    );
    assert!(
        frame_owner_size_is_derived(&built),
        "FRAME_OWNER 区尺寸与页数必须由 size_of::<OwnerIndex>() 与 PAGE_SIZE 推导"
    );
}

#[test]
fn owner_index_layout_is_c_and_documented() {
    let pmm = read(PMM_RS);
    let body = fn_body(&pmm, "struct OwnerIndex {");
    assert!(
        attr_before_sig(&pmm, "struct OwnerIndex {", "#[repr(C)]"),
        "OwnerIndex 经 *mut u8 区段指针转接, 字段顺序必须与内存布局一致"
    );
    assert!(
        body.contains("alloc_site") && body.contains("release_site"),
        "所有权槽必须同时含分配点与归还点 (缺一即三元组不完整)"
    );
}

// ============================================================================
// 不变式 2: 归因写入点唯一且粒度正确
// ============================================================================

#[test]
fn alloc_attribution_only_at_pmm_alloc_entries() {
    let pmm = read(PMM_RS);
    let prod = prod_code(&pmm);
    assert_eq!(
        count_alloc_attribution_sites(&prod),
        2,
        "分配点归因必须恰在 alloc_page / alloc_pages 两处成功分支; 增多说明有人在包装层重复记账, \
         减少说明某条分配路径脱离归因"
    );
}

#[test]
fn retire_attribution_is_wired_at_chain_enqueue() {
    let df = read(DEFERRED_FREE_RS);
    let body = fn_body(&df, "pub(crate) fn defer_free");
    assert!(
        has_retire_attribution_at_enqueue(&body),
        "退役点必须记在 defer_free 入链处; 只记 free_page 时其调用点恒为 free_chain, 归因为零"
    );
}

#[test]
fn table_frame_release_still_passes_single_zero_gate() {
    // 所有权元数据是观测面, 不得取代既有单一归零门 (frame_dec 归零才入链).
    let mm_mod = read(MM_MOD_RS);
    let body = fn_body(&mm_mod, "pub(crate) fn release_table_frame_locked");
    assert!(
        body.contains("frame_dec(phys)") && body.contains("deferred_free::defer_free(phys.0)"),
        "release_table_frame_locked 必须先经 frame_dec 归零门再入链, 所有权机制不得绕过"
    );
}

// ============================================================================
// 不变式 3: track_caller 穿透到底
// ============================================================================

#[test]
fn pmm_alloc_free_entries_are_track_caller() {
    let pmm = read(PMM_RS);
    for sig in [
        "pub fn alloc_page(&self)",
        "pub fn alloc_pages(&self, count: usize)",
        "pub fn free_page(&self, addr: PhysAddr)",
        "pub fn free_pages(&self, addr: PhysAddr, count: usize)",
        "pub fn alloc_huge_page(&self, size_type: PageSize)",
        "pub fn free_huge_page(&self, addr: PhysAddr, size_type: PageSize)",
    ] {
        assert!(
            has_track_caller(&pmm, sig),
            "{sig} 必须标 #[track_caller], 否则归因落到 pmm.rs 自身"
        );
    }
    assert!(
        has_track_caller(&pmm, "fn do_free(&self, addr: PhysAddr, order: u8)"),
        "do_free 必须标 #[track_caller]: 三元组的 this_free 字段靠它取本次归还点"
    );
}

#[test]
fn phys_wrappers_are_track_caller() {
    let api = read(API_RS);
    for sig in [
        "pub fn pmm_alloc_page_phys()",
        "pub fn pmm_free_page_phys(addr: super::PhysAddr)",
        "pub fn pmm_alloc_pages_phys(count: usize)",
        "pub fn pmm_free_pages_phys(addr: super::PhysAddr, count: usize)",
        "pub fn pmm_alloc_huge_page_phys(size_type: super::PageSize)",
    ] {
        assert!(
            has_track_caller(&api, sig),
            "{sig} 必须标 #[track_caller]: 包装层吞掉调用点等于把归因写死成 api.rs"
        );
    }
}

// ============================================================================
// 不变式 4: 异常归还早退不入链
// ============================================================================

#[test]
fn double_free_warns_with_triple_and_bails_out() {
    let pmm = read(PMM_RS);
    let body = fn_body(&pmm, "fn do_free(&self, addr: PhysAddr, order: u8)");
    let branch = double_free_branch(&body);
    assert!(
        double_free_bails_out(&branch),
        "二次释放分支必须打印 alloc=/first_retire=/this_free= 三元组并早退, 不得触碰位图或空闲链"
    );
}

// ============================================================================
// 不变式 5: 观测面不打挂控制流
// ============================================================================

#[test]
fn retire_observer_never_panics() {
    let pmm = read(PMM_RS);
    let body = fn_body(
        &pmm,
        "pub(crate) fn note_frame_retired(frame: u64, site: u64)",
    );
    assert!(
        observer_never_panics(&body),
        "退役归因入口必须软取单例: PMM 未建立时静默跳过 (host 测试仅测链逻辑)"
    );
}

#[test]
fn retire_writer_holds_lock_and_bounds() {
    let pmm = read(PMM_RS);
    let body = fn_body(&pmm, "pub(crate) fn note_frame_retired_site");
    assert!(
        retire_attribution_holds_pmm_lock(&body),
        "载体读写按 MetaStore 契约须在 PMM 锁下; 记录函数必须自持锁 (与 frame_dec 同法)"
    );
    assert!(
        retire_attribution_bounds_checked(&body),
        "pfn 越界 (MMIO / 非 PMM 地址) 必须 fail-closed, 否则越界写所有权区"
    );
}

// ============================================================================
// 不变式 6: 双载体同构
// ============================================================================

#[test]
fn owner_methods_have_trait_plus_two_impls() {
    let pmm = prod_squashed(PMM_RS);
    assert!(
        owner_methods_are_symmetric(&pmm),
        "所有权方法必须 trait 声明 1 份 + RawMetaStore / VecMetaStore 各 1 份实现"
    );
}

#[test]
fn raw_store_owner_accessors_skip_when_unready() {
    // RawMetaStore 的载体在 init_bitmap 前为 None: 静默跳过是唯一正确行为
    // (早期分配走线性分配器, 帧本就未经 PMM 计数与归因).
    let pmm = prod_squashed(PMM_RS);
    assert_eq!(
        count(&pmm, "letSome(owner)=self.ownerelse{return};"),
        2,
        "owner_mark_alloc / owner_mark_release 都必须在载体未就绪时静默跳过"
    );
}

// ============================================================================
// 负控制: 谓词必须能拒绝合成回退样本
// ============================================================================

#[test]
fn test_negative_control_predicates_reject_regressions() {
    // 1. FRAME_OWNER 区被删 (回到四区布局).
    let no_region = "let counts_bytes=total_pages;store.setup_counts(counts_phys,counts_bytes);";
    assert!(
        !has_frame_owner_region(&squash(no_region)),
        "负控制失效: 缺 FRAME_OWNER 区仍被判通过"
    );

    // 2. 退役归因写回 free_page (归因恒为 free_chain).
    let df_no_enqueue = "BATCH_HEAD.store(frame, Ordering::Release);";
    assert!(
        !has_retire_attribution_at_enqueue(df_no_enqueue),
        "负控制失效: defer_free 未记退役点仍被判通过"
    );

    // 3. 二次释放改成"照常入链"(链表成环的经典形态).
    let no_bail = "klog_pmm!(\"double free alloc={}:{} first_retire={}:{} this_free={}:{}\"); \
                   self.clear_bit(pfn as usize); self.buddy_list_push(pfn, order); self.stats_free(1);"
        .to_string();
    assert!(
        !double_free_bails_out(&no_bail),
        "负控制失效: 二次释放仍触碰空闲链却被判通过"
    );
    // 3b. 只报帧号不报三元组 (旧告警形态).
    assert!(
        !double_free_bails_out("klog_pmm!(\"[PMM] Warn: double free at pfn {}\"); return;"),
        "负控制失效: 缺三元组的旧告警形态仍被判通过"
    );

    // 4. 观测入口用硬取单例 (host 测试下 panic).
    assert!(
        !observer_never_panics("get_pmm().note_frame_retired_site(PhysAddr(frame), site);"),
        "负控制失效: 会 panic 的观测入口仍被判通过"
    );

    // 5. 记录函数丢锁 (裸写元数据).
    assert!(
        !retire_attribution_holds_pmm_lock("store.owner_mark_release(pfn, site);"),
        "负控制失效: 不持 PMM 锁的写入仍被判通过"
    );

    // 6. VecMetaStore 漏实现 (host 侧覆盖不到该语义).
    let asymmetric = "fn setup_owner(&mut self){}fn setup_owner(&mut self){}fn owner_read(&self){}\
                      fn owner_mark_alloc(&self){}fn owner_mark_release(&self){}";
    assert!(
        !owner_methods_are_symmetric(&squash(asymmetric)),
        "负控制失效: 载体实现不对称仍被判通过"
    );

    // 7. 分配点被包装层重复记 (计数与归因粒度漂移).
    let prod = prod_code(&read(PMM_RS));
    assert_eq!(
        count_alloc_attribution_sites("self.note_frame_alloc(a);self.note_frame_alloc(b);"),
        2,
        "负控制基线: 合成两站点应计为 2 (真实文件亦须恰为 2, 见上一个用例)"
    );
    assert_eq!(
        count_alloc_attribution_sites("self.note_frame_alloc(a);"),
        1,
        "负控制失效: 少一处归因未被察觉"
    );
    assert!(
        count_alloc_attribution_sites(&prod) != 0,
        "真实文件必须至少有一处归因 (谓词本身不能恒假)"
    );
}
