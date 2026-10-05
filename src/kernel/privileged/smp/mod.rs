//! SMP (对称多处理) 支持
//!
//! 多处理器初始化、IPI 与每 CPU 状态管理.

use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

static SMP_ENABLED: AtomicBool = AtomicBool::new(false);
static CPU_COUNT: AtomicU32 = AtomicU32::new(1);

/// 各逻辑 CPU 槽位登记的**硬件 id** (`MAX_CPUS` 槽; 未占用为哨兵 `0xFFFF`)。
///
/// - x86_64: Local APIC id;
/// - aarch64: 紧凑亲和 id `(Aff3<<24)|(Aff2<<16)|(Aff1<<8)|Aff0`。
///
/// 这是逻辑索引 ↔ 硬件 id 的**唯一**映射源: `CPU_HW_IDS[逻辑索引] == 硬件 id`。
/// 逻辑索引由 [`register_cpu`] 按上线次序稠密分配, 与 [`CPU_ONLINE`]/[`CPU_TLB_GEN`]
/// 等 per-CPU 数组下标一致; 由 [`current_cpu_index`] 线性扫描本表反查本核索引。
static CPU_HW_IDS: [AtomicU32; crate::privileged::config::MAX_CPUS] =
    [const { AtomicU32::new(0xFFFF) }; crate::privileged::config::MAX_CPUS];

static CPU_ONLINE: [AtomicBool; crate::privileged::config::MAX_CPUS] =
    [const { AtomicBool::new(false) }; crate::privileged::config::MAX_CPUS];

// 全局 TLB 失效代: 单调递增, 由页表修改批次在其末尾发布.
static TLB_GEN: AtomicU64 = AtomicU64::new(0);

// 每核"已追平代": 本核最后一次全量 TLB 失效时观察到的 TLB_GEN 值.
static CPU_TLB_GEN: [AtomicU64; crate::privileged::config::MAX_CPUS] =
    [const { AtomicU64::new(0) }; crate::privileged::config::MAX_CPUS];

/// `tlb_gen_publish_and_shoot` 被调用的次数, 即真实的跨核 TLB 失效批次次数.
///
/// 仅用于可观测性/测试断言 (Makefile 依据打印行统计), 不参与任何控制逻辑.
static TLB_SHOOTDOWN_COUNT: AtomicU64 = AtomicU64::new(0);

#[expect(
    clippy::ptr_as_ptr,
    reason = "指针类型 cast 不变 constness (e.g. *mut T → *mut U); 改 .cast() 是机械替换不治根, 当前优先 expect 兑底"
)]
pub fn init() {
    let bsp_hw_id = crate::arch!(cpu_id());
    CPU_HW_IDS[0].store(bsp_hw_id, Ordering::Release);
    CPU_ONLINE[0].store(true, Ordering::Release);
    CPU_COUNT.store(1, Ordering::Release);

    // SAFETY: 调用方保证指针/类型有效 (详见上下文)
    unsafe {
        crate::privileged::klog::klog_info(c"[SMP] BSP initialized".as_ptr() as *const i8);
    }
}

pub fn is_enabled() -> bool {
    SMP_ENABLED.load(Ordering::Acquire)
}

pub fn get_cpu_count() -> u32 {
    CPU_COUNT.load(Ordering::Acquire)
}

/// 返回当前 CPU 的**逻辑索引** (`[0, get_cpu_count())`)。
///
/// 读取本核硬件 id (`arch::cpu_id()`) 后在 [`CPU_HW_IDS`] 的已登记前缀内线性
/// 扫描反查; 未命中 (例如极早期调用, 槽位尚未登记) 回退 BSP (0), 保证返回值
/// 恒为有效索引。核数上限 [`crate::privileged::config::MAX_CPUS`] 有界, 扫描
/// 成本可接受且为纯读, 可在中断上下文调用。
pub fn current_cpu_index() -> u32 {
    let hw_id = crate::arch!(cpu_id());
    let count = get_cpu_count();
    let mut i = 0u32;
    while i < count {
        if CPU_HW_IDS[i as usize].load(Ordering::Acquire) == hw_id {
            return i;
        }
        i += 1;
    }
    0
}

/// 返回当前 CPU 的逻辑索引 (等价于 [`current_cpu_index`])。
///
/// 语义修正 (RT-002): 本函数历史上直接返回硬件 id (x86: LAPIC id / aarch64:
/// MPIDR Aff0), 但全部内核调用点都把它当**稠密逻辑索引**用于 per-CPU 数组寻址
/// (调度器 / softirq / RCU / CpuQueue / TLB 代), 稀疏拓扑下二者不一致会导致
/// 索引错位。现统一返回逻辑索引。
pub fn get_current_cpu() -> u32 {
    current_cpu_index()
}

/// 登记一个上线 CPU: 按当前计数器稠密分配逻辑索引, 并把其硬件 id 存入该槽。
///
/// 返回 `false` 表示槽位耗尽 (超过 `MAX_CPUS`), 计数器已回滚。
pub fn register_cpu(hw_id: u32) -> bool {
    let count = CPU_COUNT.fetch_add(1, Ordering::AcqRel);
    if count as usize >= crate::privileged::config::MAX_CPUS {
        CPU_COUNT.fetch_sub(1, Ordering::AcqRel);
        return false;
    }

    CPU_HW_IDS[count as usize].store(hw_id, Ordering::Release);
    CPU_TLB_GEN[count as usize].store(TLB_GEN.load(Ordering::Acquire), Ordering::Release);
    CPU_ONLINE[count as usize].store(true, Ordering::Release);
    SMP_ENABLED.store(true, Ordering::Release);
    true
}

pub fn is_cpu_online(cpu_index: u32) -> bool {
    if cpu_index as usize >= crate::privileged::config::MAX_CPUS {
        return false;
    }
    CPU_ONLINE[cpu_index as usize].load(Ordering::Acquire)
}

/// 返回逻辑索引 `cpu_index` 对应槽位登记的硬件 id; 越界返哨兵 `0xFFFF`。
pub fn get_hw_id(cpu_index: u32) -> u32 {
    if cpu_index as usize >= crate::privileged::config::MAX_CPUS {
        return 0xFFFF;
    }
    CPU_HW_IDS[cpu_index as usize].load(Ordering::Acquire)
}

/// 向逻辑索引 `cpu_index` 的核发送 TLB 失效 IPI (vector `0xFD` / SGI 13)。
///
/// 以**逻辑索引**为键: 内部经 [`get_hw_id`] 解析目标硬件 id 后交由架构层寻址。
/// 目标未登记 (哨兵 `0xFFFF`) 时静默返回。
pub fn send_tlb_invalidate_ipi(cpu_index: u32) {
    let hw_id = get_hw_id(cpu_index);
    if hw_id == 0xFFFF {
        return;
    }
    crate::arch!(send_ipi(hw_id, 0xFD));
}

pub fn send_broadcast_ipi(vector: u8) {
    crate::arch!(broadcast_ipi(vector));
}

pub fn broadcast_tlb_invalidate() {
    if is_enabled() {
        send_broadcast_ipi(0xFD);
    }
}

/// 向逻辑索引 `cpu_index` 的核发送重新调度 IPI (vector `0xFE` / SGI 14)。
///
/// 以**逻辑索引**为键: 内部经 [`get_hw_id`] 解析目标硬件 id 后交由架构层寻址。
/// 目标未登记 (哨兵 `0xFFFF`) 时静默返回。
pub fn send_reschedule_ipi(cpu_index: u32) {
    let hw_id = get_hw_id(cpu_index);
    if hw_id == 0xFFFF {
        return;
    }
    crate::arch!(send_ipi(hw_id, 0xFE));
}

pub fn broadcast_reschedule() {
    if is_enabled() {
        send_broadcast_ipi(0xFE);
    }
}

/// 读取当前全局 TLB 失效代.
///
/// 供接收侧在 flush **之前**调用: 先读到当代, 待全量失效完成后再以
/// [`tlb_gen_set_self`] 声明已追平该代; 次序不可颠倒, 反序会"假追平".
pub fn tlb_gen_now() -> u64 {
    TLB_GEN.load(Ordering::Acquire)
}

/// 发布新代并返回新代值 (单调递增).
///
/// 页表修改批次在其末尾调用; 调用方随后须通知全部在线核失效.
pub fn tlb_gen_publish() -> u64 {
    TLB_GEN.fetch_add(1, Ordering::AcqRel) + 1
}

/// 声明本核已追平代 `g`.
///
/// 接收侧完成全量 TLB 失效后调用, `g` 须为 flush **之前**读到的代;
/// 核索引越界时直接返回 (不得 panic), 越界核不参与 [`tlb_gen_min_online`] 统计.
pub fn tlb_gen_set_self(g: u64) {
    let cpu = get_current_cpu() as usize;
    if cpu >= crate::privileged::config::MAX_CPUS {
        return;
    }
    CPU_TLB_GEN[cpu].store(g, Ordering::Release);
}

/// 返回全部**在线**核已追平代的最小值.
///
/// 仅统计 `CPU_ONLINE[i] == true` 的槽位: 离线槽位不得参与取 min, 否则最小值会
/// 永久卡在旧值. 若一个在线槽位都没有, 返回 `u64::MAX` (fail-closed: 宁可永不
/// 判定"可释放", 也绝不误判为可释放).
pub fn tlb_gen_min_online() -> u64 {
    let cpu_count = get_cpu_count();
    let mut min_gen = u64::MAX;
    for i in 0..cpu_count {
        if CPU_ONLINE[i as usize].load(Ordering::Acquire) {
            let g = CPU_TLB_GEN[i as usize].load(Ordering::Acquire);
            if g < min_gen {
                min_gen = g;
            }
        }
    }
    min_gen
}

/// 收尾一次跨核 TLB 失效接收序列: 读代 → 全量失效本核 TLB → 声明本核已追平.
///
/// x86_64 (`0xFD`) 与 aarch64 (SGI 13) 接收侧共用的**唯一**实现 (消除平行实现).
/// 三段次序不可颠倒: 先 flush 后读代会读到 flush 之后新发布的代, 把本次 flush
/// 未覆盖的批次误判为已追平 (假追平). 该次序由
/// `scripts/audit_tlb_receive_order.py` 静态 fail-closed 强制.
pub fn tlb_catch_up_local() {
    let g = tlb_gen_now();
    crate::privileged::mm::arch::tlb_flush_all();
    tlb_gen_set_self(g);
}

/// 发布新代并向全部在线核 (含本核) 发送 `0xFD` 定向 IPI, 返回新代值.
///
/// 发布必须先于发 IPI: 对端收到 IPI 即会读代, 反序会读到旧代. IPI 集合含本核,
/// 使发送侧与接收侧走完全相同的"读代 → 全量 flush → 声明"路径. 本函数**不等待**:
/// 不引入任何自旋或 ack, 并发发布各自得到不同代, 无需互斥.
pub fn tlb_gen_publish_and_shoot() -> u64 {
    let g = tlb_gen_publish();
    let cpu_count = get_cpu_count();
    let mut targets = 0u64;
    for i in 0..cpu_count {
        if CPU_ONLINE[i as usize].load(Ordering::Acquire) {
            send_tlb_invalidate_ipi(i);
            targets += 1;
        }
    }
    // SIMPLIFIED: 每次 shootdown 都打印一行日志 (无采样/无阈值); 高频 unmap 负载下串口输出
    // 量线性增长, 可能拖慢该路径; 当 shootdown 成为热路径或日志刷屏时, 改为首次/每 N 次采样打印.
    let count = TLB_SHOOTDOWN_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
    crate::klog_info!(
        Kernel,
        "[SMP] TLB shootdown #{} gen={} targets={}",
        count,
        g,
        targets
    );
    g
}

// ── 运行期跨核 TLB 失效探针 (S-13) ───────────────────────────────────────
//
// 判别力来源: 探针**不比对 shootdown 计数**, 而是要求每个远程核在 0xFD/SGI13
// 接收路径内部读一个专用探测页并按 (代, 观测字节) 报告. 因此:
// - 令 `flush_tlb_remote` 不置位 (不发 IPI) ⇒ 远程核永不报告 ⇒ 超时判 FAIL;
// - 令定向 IPI 未送达 ⇒ 同上;
// - 令 `tlb_flush_all` 退化为空操作 ⇒ 远程核报告陈旧字节 (帧 A 内容) ⇒ 判 FAIL.
// 见 docs/plan/tlb-shootdown-epoch.md §5 注入 3.

/// 探针当前代: 0 = 未装备 (接收侧此时不触碰探测页).
static TLB_PROBE_SEQ: AtomicU64 = AtomicU64::new(0);

/// 每核最近一次探针观测到的**代**: 0 = 尚无观测.
static TLB_PROBE_OBS_SEQ: [AtomicU64; crate::privileged::config::MAX_CPUS] =
    [const { AtomicU64::new(0) }; crate::privileged::config::MAX_CPUS];

/// 每核最近一次探针观测到的**字节**.
///
/// 写者次序: 先写本槽, 后写 [`TLB_PROBE_OBS_SEQ`] (Release); 读者据代判定字节有效.
static TLB_PROBE_OBS_VAL: [AtomicU32; crate::privileged::config::MAX_CPUS] =
    [const { AtomicU32::new(0) }; crate::privileged::config::MAX_CPUS];

/// 装备探针 (驱动侧调用): 清空全部在线核观测槽后发布新代 `seq`.
///
/// 必须**逐个代串行**使用: 驱动方须等上一代全部远程核报告完毕, 再装备下一代,
/// 否则迟到的旧代报告会覆盖新代观测槽 (接收侧登记的是"装备时读到的代").
pub fn tlb_probe_arm(seq: u64) {
    let cpu_count = get_cpu_count();
    for i in 0..(cpu_count as usize).min(crate::privileged::config::MAX_CPUS) {
        TLB_PROBE_OBS_SEQ[i].store(0, Ordering::Relaxed);
        TLB_PROBE_OBS_VAL[i].store(0, Ordering::Relaxed);
    }
    TLB_PROBE_SEQ.store(seq, Ordering::Release);
}

/// 收起探针: 接收侧此后不再触碰探测页 (拆除映射前必须先收起).
pub fn tlb_probe_disarm() {
    TLB_PROBE_SEQ.store(0, Ordering::Release);
}

/// 登记探针观测 (由接收侧在 [`tlb_catch_up_local`] **之后**调用).
///
/// 仅登记, 不 panic, 不做任何失效判定 (判定在驱动侧
/// [`tlb_probe_wait_remotes`]); 目的只是把"本核确实执行了 flush 之后所读到的
/// 字节"暴露给驱动侧.
pub fn tlb_probe_report() {
    let seq = TLB_PROBE_SEQ.load(Ordering::Acquire);
    if seq == 0 {
        return;
    }
    let cpu = get_current_cpu() as usize;
    if cpu >= crate::privileged::config::MAX_CPUS {
        return;
    }
    // SAFETY: `tlb_probe_arm` 之后探测页在内核页表中恒为 present (驱动方保证
    // "先建映射, 后装备"), 且全部在线核共用同一内核页表; 读取页对齐首字节不越界.
    let v = unsafe { core::ptr::read_volatile(crate::privileged::mm::TLB_PROBE_VA as *const u8) };
    TLB_PROBE_OBS_VAL[cpu].store(u32::from(v), Ordering::Relaxed);
    TLB_PROBE_OBS_SEQ[cpu].store(seq, Ordering::Release);
}

/// 统计 [0, cpu_count) 内**远程在线核**尚未报告 `(seq, expect)` 的个数.
fn tlb_probe_missing(seq: u64, expect: u8, me: u32, cpu_count: u32) -> u32 {
    let mut missing = 0u32;
    for i in 0..cpu_count {
        if i == me || !CPU_ONLINE[i as usize].load(Ordering::Acquire) {
            continue;
        }
        if TLB_PROBE_OBS_SEQ[i as usize].load(Ordering::Acquire) != seq
            || TLB_PROBE_OBS_VAL[i as usize].load(Ordering::Acquire) != u32::from(expect)
        {
            missing += 1;
        }
    }
    missing
}

/// 有界自旋等待全部**远程在线核**报告 `(seq, expect)`, 返回仍未达成的核数.
///
/// 不含本核: 驱动侧在重映射前已自行失效本核 TLB, 且引导期 BSP 中断可能关闭,
/// 自身 IPI 未必即时投递 —— 本核不构成"跨核传播"的证据.
/// fail-closed: 超时后把未达成核数原样返回 (非 0 即失败), 不以"待定"放行.
pub fn tlb_probe_wait_remotes(seq: u64, expect: u8) -> u32 {
    // 有界自旋: AP 处于 `sti; hlt` 空闲态, IPI 投递为微秒级; 该上界远超所需,
    // 仅在 IPI 真正未送达 (探针要判定的故障) 时耗尽.
    const TIMEOUT_SPINS: u32 = 50_000_000;
    let me = get_current_cpu();
    let cpu_count = get_cpu_count();
    let mut spins = 0u32;
    while spins < TIMEOUT_SPINS {
        if tlb_probe_missing(seq, expect, me, cpu_count) == 0 {
            return 0;
        }
        core::hint::spin_loop();
        spins += 1;
    }
    tlb_probe_missing(seq, expect, me, cpu_count)
}

// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn smp_init() {
    init();
}
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn smp_is_enabled() -> bool {
    is_enabled()
}
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn smp_get_cpu_count() -> u32 {
    get_cpu_count()
}
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn smp_get_current_cpu() -> u32 {
    get_current_cpu()
}
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn smp_register_cpu(hw_id: u32) -> bool {
    register_cpu(hw_id)
}
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn smp_send_tlb_invalidate_ipi(cpu_index: u32) {
    send_tlb_invalidate_ipi(cpu_index);
}
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn smp_broadcast_tlb_invalidate() {
    broadcast_tlb_invalidate();
}
// SAFETY: FFI 导出函数，通过 C ABI 与外部代码互操作
#[unsafe(no_mangle)]
pub extern "C" fn smp_send_reschedule_ipi(cpu_index: u32) {
    send_reschedule_ipi(cpu_index);
}
