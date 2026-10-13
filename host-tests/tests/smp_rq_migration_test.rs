//! SMP 运行队列归属协议回归锁 (E-II-b 后续修复)
//!
//! # 被锁死的缺陷 (取证结论)
//!
//! E-II-b 把 x86_64 的 tick 源从"PIT → 只有 BSP 收"改成"每核 LAPIC 私有定时器",
//! 于是 `Scheduler::tick` 里按"全内核只有一个核收 tick"写成的周期性全局副作用
//! 第一次在多核上被激活。x86_64 e2e 由 6/6 退化为 1/6, 失败特征是**纯 RX 侧全灭**
//! (`[net6]`/`[mcast]`/`[udp] recvfrom` 超时, `[tcp] bind` 失败) 而 TX 与 DHCP 正常
//! —— 是状态被踩坏, 不是时序抖动。
//!
//! 根因是跨核偷取的**归属真空窗口**: 旧 `load_balance` 先持源 rq 锁 `pick_next` +
//! `dequeue` + 清 `cfs_on_rq`, **放锁**, 再走 `cfs_enqueue_to` 投目的核。窗口里任务
//! 既不在源树也不在目的树, 且凭证为 `on_rq=false / state=Ready` —— 任何并发唤醒看
//! 到的正是"Ready 且不在队"(与 `unblock` 的补投判据同形), 于是把它投进第三个队列;
//! 窃取者随后又投进目的队列 ⇒ **同一任务同时挂在两棵 rq 树上, 两核同时上核运行它**,
//! 其进程上下文与 socket 状态随即被踩坏。
//!
//! 同一个 `enqueue` 面还有第二处缺口: 它只持目的核的锁, 遇到"任务仍挂在别的核"时
//! 既不去重也不搬账, 所以并发唤醒本身就能造出双树成员。
//!
//! # 修复形态 (= 本测试的断言对象)
//!
//! 1. **归属显式化**: `Process::cfs_rq_cpu` 记录"可运行记账在哪棵树上"
//!    (语义同 Linux `task_cpu(p)` 配合 `on_rq`), 与 `cfs_on_rq` 是一对同步更新的凭证。
//! 2. **搬动先认领**: 一切归属变更先经 `MIGRATE_PENDING` CAS 认领; 认领**非阻塞**
//!    (等待者可能正握着认领者需要的 rq 锁, 一等待即 AB-BA)。
//! 3. **单入口 + 双持锁**: 入队/迁移一律经 `cfs_place_on`, 源与目的的 rq 锁按
//!    cpu index **升序**双持, 摘源 + 投目的 + 搬账 + 写回归属在同一段临界区内闭合。
//! 4. **凭证与归属同清**: 任何把 `cfs_on_rq` 置 false 的路径必须同时把归属写回
//!    `NO_RQ_CPU`, 否则下一次按陈旧归属撤账会把同一份权重再扣一次。
//! 5. **tick 体每窗口单名额**: 全局周期副作用 (kswapd / hotplug / reap / balance)
//!    一律 carry + CAS, 禁用 `is_multiple_of` —— 各核看到同一个派生 tick 时,
//!    只比时间窗不夺名额即各核同窗各自发起同一轮全局副作用。
//!
//! # 断言方式
//!
//! `CfsRunQueue` 是真能独立跑的纯数据结构, 所以账目守恒/候选有序这类**行为**用真
//! 对象测; 而"认领非阻塞""双持锁序""归属同清"是**跨函数形状**, 主机侧无法构造双核
//! 竞态 (无真 SMP 内核上下文), 只能用源码文本契约锁死 (剥注释 + 去空白, 只锁语义
//! 形状, 不受 rustfmt 换行影响)。文本契约的判别力由 [`test_negative_control_predicates`]
//! 用合成回退样本反向验证。

use edgine::kernel::privileged::proc::cfs::CfsRunQueue;
use std::fs;
use std::sync::atomic::Ordering;

const SCHED: &str = "../src/kernel/privileged/proc/scheduler.rs";
const PROCESS: &str = "../src/kernel/privileged/proc/process.rs";

// ============================================================================
// 文本读取与规范化
// ============================================================================

fn read(path: &str) -> String {
    fs::read_to_string(path).unwrap_or_else(|e| panic!("读取 {path} 失败: {e}"))
}

/// 剥除注释行: 生产代码与负控制样本走同一路径 (本轮修复在多处把被禁形态写进注释).
fn strip_comments(src: &str) -> String {
    src.lines()
        .map(str::trim)
        .filter(|l| !l.starts_with("//") && !l.starts_with("/*") && !l.starts_with('*'))
        .collect::<Vec<_>>()
        .join("\n")
}

/// 在测试模块声明处截断 (`#[cfg(test)]` 紧接 `mod` 时), 使内核内联用例不污染契约.
fn truncate_test_module(src: &str) -> String {
    let lines: Vec<&str> = src.lines().collect();
    for (i, line) in lines.iter().enumerate() {
        if line.starts_with("#[cfg(test)]") || line.starts_with("#[cfg(all(test") {
            let next = lines.get(i + 1).unwrap_or(&"");
            if next.starts_with("mod ") || next.starts_with("pub mod ") {
                return lines[..i].join("\n");
            }
        }
    }
    src.to_string()
}

fn squash(code: &str) -> String {
    code.chars().filter(|c| !c.is_whitespace()).collect()
}

fn prod_squashed(path: &str) -> String {
    squash(&truncate_test_module(&strip_comments(&read(path))))
}

/// 取 `sig` 函数体的文本 (花括号配对; 输入须已剔注释并 squash).
///
/// sig 写 squash 后的形态 (无空格), 使其只命中定义而不命中调用点.
fn fn_body<'a>(code: &'a str, sig: &str) -> Option<&'a str> {
    let at = code.find(sig)?;
    let open = code[at..].find('{')? + at;
    let mut depth = 0usize;
    for (i, ch) in code[open..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&code[open..open + i + 1]);
                }
            }
            _ => {}
        }
    }
    None
}

// ============================================================================
// 行为层: CfsRunQueue 的账目与候选语义 (搬动协议的正确性前提)
// ============================================================================

#[test]
fn test_leftmost_pids_ordered_and_bounded() {
    let mut rq = CfsRunQueue::new();
    rq.enqueue(7, 30, 10, false);
    rq.enqueue(5, 10, 10, false);
    rq.enqueue(9, 20, 10, false);

    // 缓冲区小于树: 只取 vruntime 升序的前 N 个, 返回实际写入数.
    let mut two = [0u32; 2];
    assert_eq!(rq.leftmost_pids(&mut two), 2, "候选数未受缓冲区约束");
    assert_eq!(two, [5, 9], "候选未按 vruntime 升序");

    // 缓冲区大于树: 不得越界读, 返回值即树上任务数.
    let mut all = [0u32; 8];
    assert_eq!(rq.leftmost_pids(&mut all), 3);
    assert_eq!(&all[..3], &[5, 9, 7]);

    let mut empty = [0u32; 4];
    assert_eq!(CfsRunQueue::new().leftmost_pids(&mut empty), 0);
}

#[test]
fn test_cross_rq_transfer_moves_accounting_exactly_once() {
    let (mut src, mut dst) = (CfsRunQueue::new(), CfsRunQueue::new());
    src.enqueue(3, 5, 100, false);
    src.enqueue(4, 6, 50, false);
    assert_eq!(src.total_weight.load(Ordering::Acquire), 150);

    // `steal_locked` 的账目动作: 源撤账摘节点, 目的重新记账投节点.
    src.dequeue(3, 100, true);
    dst.enqueue(3, 5, 100, false);
    assert_eq!(src.total_weight.load(Ordering::Acquire), 50);
    assert_eq!(dst.total_weight.load(Ordering::Acquire), 100);

    // 幂等: 已搬走者被同一目的核重复投送时, 凭证为"已记账"即不二次加账.
    dst.enqueue(3, 5, 100, true);
    assert_eq!(dst.total_weight.load(Ordering::Acquire), 100);

    // 反面事实 (归属凭证为何必须与记账同清): 对已不在本树的任务按 accounted=true
    // 撤账, 权重就被凭空扣掉 —— 正是"陈旧归属再撤一次"的账目后果.
    dst.dequeue(3, 100, true);
    dst.dequeue(3, 100, true);
    assert_eq!(dst.total_weight.load(Ordering::Acquire), 0);
    // 而未记账者不动账: `dequeue(.., false)` 是幂等的.
    src.dequeue(99, 1000, false);
    assert_eq!(src.total_weight.load(Ordering::Acquire), 50);
}

// ============================================================================
// 形状层: 不变式 1 + 4 (归属显式化 + 凭证与归属同清)
// ============================================================================

#[test]
fn test_ownership_field_is_declared_and_sentinel_coded() {
    let proc_c = prod_squashed(PROCESS);
    assert!(
        proc_c.contains("pubconstNO_RQ_CPU:u32=u32::MAX;"),
        "缺少\"不在任何队列上\"归属哨兵"
    );
    assert!(
        proc_c.contains("pubconstMIGRATE_PENDING:u32=u32::MAX-1;"),
        "缺少归属认领中间态 (无它即无法把\"无人搬动\"表达成互斥)"
    );
    assert!(
        proc_c.contains("pubcfs_rq_cpu:AtomicU32,")
            && proc_c.contains("cfs_rq_cpu:AtomicU32::new(super::process::NO_RQ_CPU),"),
        "Process 未携带 cfs_rq_cpu 归属字段或初值不是 NO_RQ_CPU"
    );
}

#[test]
fn test_credential_clear_always_paires_with_owner_clear() {
    let sched = prod_squashed(SCHED);
    // 三条"离开可运行集"的路径: 清记账凭证时必须把归属一并归零 —— 但**不得**直接
    // 写归属字段, 必须经认领凭证 (commit), 否则并发搬动者手里的 `MIGRATE_PENDING`
    // 会被覆写成陈旧值.
    for (sig, why) in [
        ("fncfs_deactivate(&self,pid:Pid)", "阻塞/不可重排撤账"),
        ("fnpick_cfs_task(&self)", "拣出但不可调度撤账"),
        ("fnsteal_locked(", "偷取时遇不可调度者撤账"),
    ] {
        let body = fn_body(&sched, sig).unwrap_or_else(|| panic!("{sig} 不存在"));
        assert!(
            body.contains("claim.commit(NO_RQ_CPU)"),
            "{why} 路径清了 on_rq 凭证却没经凭证归零归属: 陈旧归属会被再撤一次账"
        );
    }

    // 结构事实 (比"成对出现"更强): 归属字段的直接写入点全文件**只允许一处** ——
    // 凭证内部的 `set_rq_cpu`. E-II-b 回归实证: 手工配对 claim/release 时,
    // `cfs_deactivate` 的 `NO_RQ_CPU` 早退分支漏一次回滚就把任务永久钉在
    // `MIGRATE_PENDING` —— 实测全内核搬定成功 4 次 / 认领失败 144369 次,
    // 唤醒全部静默丢失, e2e 卡在"子进程一阻塞就再也不上核".
    assert_eq!(
        sched.matches("cfs_rq_cpu.store(").count(),
        1,
        "归属字段出现第二个直接写入点: PENDING 必须被回滚这条义务重新退化成人工约定"
    );
    // 凭证必须在未提交时自动回滚, 否则"泄漏不可表达"不成立.
    assert!(
        sched.contains("implDropforRqClaim") && sched.contains("if!self.committed"),
        "认领凭证缺 Drop 回滚: 任何早退分支即永久泄漏 MIGRATE_PENDING"
    );
    assert!(
        sched.contains("fncommit(&mutself,cpu_id:u32)") && sched.contains("self.committed=true"),
        "认领凭证缺 commit 提交面: 归属无法在搬定成功后写定"
    );
}

/// 不变式 6: 唤醒侧不可静默失败 —— 置 Ready 必须后于搬定成功.
///
/// E-II-b 回归另一面: `unblock` 在判据阶段就把 Blocked 改成 Ready, 再调可失败的
/// 搬定入口. 一旦搬定失败, 该任务既不在树上, 也不再满足唤醒面重扫的选集条件
/// (`state == Blocked`), 电平重扫自愈被打断 —— 一次可恢复的丢唤醒变永久隐形.
#[test]
fn test_wake_path_never_flips_state_before_placement() {
    let sched = prod_squashed(SCHED);
    let unblock = fn_body(&sched, "pubfnunblock(&self,pid:Pid)").expect("unblock 不存在");
    let at_place = unblock
        .find("self.cfs_enqueue_to(")
        .expect("unblock 的 CFS 路由未经搬定入口");
    // 全由搬定写 Ready 也合法: find 发 None 时无需断言.
    if let Some(at_ready) = unblock.find("set_state_safe(ProcessState::Ready)") {
        assert!(
            at_ready > at_place,
            "unblock 在搬定前就置 Ready: 搬定失败即永久隐形 (电平重扫选集条件丢开它)"
        );
    }
    // 搬定失败必须早退 (不能当作已成功继续跑唤醒不变式).
    assert!(
        unblock.contains("if!self.cfs_enqueue_to("),
        "unblock 未根据搬定结果决定后续 (失败被静默忽略)"
    );
    // 成功路径的 Ready 由持锁主体写.
    let place = fn_body(&sched, "fnplace_locked(").expect("place_locked 不存在");
    assert!(
        place.contains("set_state_safe(ProcessState::Ready)"),
        "搬定成功却不置 Ready: 唤醒语义丢失"
    );
    // 搬定层不得再卡 policy 准入 (准入归 pick_cfs_task / steal_locked):
    // `schedule()` 的 RT 降级分支 `_ =>` 会把非 Fifo/Rr 条目投进 CFS, 在搬定层
    // 卡住即两头皆空 (条目已从 rt_queue 弹出).
    assert!(
        !place.contains("policy!=SchedPolicy::Normal"),
        "搬定层重新卡 policy 准入: RT 降级入队与唤醒投送会静默失效"
    );
    let pick = fn_body(&sched, "fnpick_cfs_task(&self)").expect("pick_cfs_task 不存在");
    assert!(
        pick.contains("p.get_sched_policy()==SchedPolicy::Normal"),
        "policy 准入必须在拣选层 (否则上面那条禁令就只是拆护栏)"
    );
}

// ============================================================================
// 形状层: 不变式 2 (搬动先认领, 且认领非阻塞)
// ============================================================================

#[test]
fn test_membership_change_requires_claim_and_claim_never_blocks() {
    let sched = prod_squashed(SCHED);
    let claim =
        fn_body(&sched, "fntry_claim_rq(&self,pid:Pid)").expect("try_claim_rq (非阻塞认领) 不存在");
    assert!(
        claim.contains("ifcur==MIGRATE_PENDING{returnNone;}")
            && claim.contains("compare_exchange(cur,MIGRATE_PENDING"),
        "认领未以\"见 PENDING 即走人\"的 CAS 表达"
    );
    // 认领内绝不等待: 等待者手里的 rq 锁可能正是认领者要拿的那把 (AB-BA).
    assert!(
        !claim.contains("spin_loop") && !claim.contains("loop{"),
        "try_claim_rq 内出现等待: 持 rq 锁调用它即死锁"
    );

    let wait = fn_body(&sched, "fnclaim_rq(&self,pid:Pid)").expect("claim_rq 不存在");
    assert!(
        wait.contains("for_in0..RQ_CLAIM_RETRY") && wait.contains("core::hint::spin_loop()"),
        "重试等待必须在 PCB 锁外 (持锁自旋会与认领者互锁)"
    );

    // 一切归属变更都经认领 (搬定主体 = cfs_place_on_inner, 公有入口只做委托).
    for (sig, needle) in [
        (
            "fncfs_place_on_inner(&self,pid:Pid,dst_cpu:u32,owner_landing:bool)",
            "self.claim_rq(pid)",
        ),
        ("fncfs_deactivate(&self,pid:Pid)", "self.claim_rq(pid)"),
        ("fnsteal_locked(", "self.try_claim_rq(pid)"),
    ] {
        let body = fn_body(&sched, sig).unwrap_or_else(|| panic!("{sig} 不存在"));
        assert!(
            body.contains(needle),
            "{sig} 未先认领归属即变更队列 ({needle})"
        );
    }
}

// ============================================================================
// 形状层: 不变式 3 (单入口 + 双持锁升序, 无归属真空窗口)
// ============================================================================

#[test]
fn test_enqueue_is_single_entry_under_ascending_double_lock() {
    let sched = prod_squashed(SCHED);

    // 旧形态的两处"绕过归属"的编程面不得复活: 直接对某核 rq 上锁后立刻投节点.
    assert!(
        !sched.contains("cfs_rq.lock().enqueue("),
        "重新出现\"只持目的锁就入队\": 任务仍挂在别的核时既不去重也不搬账"
    );
    assert!(
        !sched.contains("cfs_rq.lock().dequeue("),
        "重新出现\"按当前核撤账\": 跨核投送后任务的家不在当前核"
    );

    // 入队唯一入口收口到 cfs_place_on.
    let enqueue_to = fn_body(&sched, "fncfs_enqueue_to(&self,pid:Pid,cpu_id:u32)")
        .expect("cfs_enqueue_to 不存在");
    assert!(
        enqueue_to.contains("self.cfs_place_on(pid,cpu_id)"),
        "cfs_enqueue_to 未委托 cfs_place_on (搬定出现第二条路径)"
    );

    // 跨核搬动双持锁, 且锁序按 cpu index 升序.
    // 搬定主体在 `cfs_place_on_inner` (多一个 `owner_landing` 参数, 见 P-21),
    // 公有入口 `cfs_place_on` 只做一层委托 —— 委托不丢, 否则等于开出第二条入口.
    let entry =
        fn_body(&sched, "fncfs_place_on(&self,pid:Pid,dst_cpu:u32)").expect("cfs_place_on 不存在");
    assert!(
        entry.contains("self.cfs_place_on_inner(pid,dst_cpu,false)"),
        "cfs_place_on 未把 owner_landing 固定为 false 再委托 (屏障可被任意入口绕过)"
    );
    let place = fn_body(
        &sched,
        "fncfs_place_on_inner(&self,pid:Pid,dst_cpu:u32,owner_landing:bool)",
    )
    .expect("cfs_place_on_inner 不存在");
    assert!(
        place.contains("ifsrc_cpu==NO_RQ_CPU||src_cpu==dst_cpu{")
            && place.contains("}elseifsrc_cpu<dst_cpu{"),
        "cfs_place_on 未把\"同锁单持 / 异锁升序双持\"分支写全 (嵌套同一把锁即自死锁)"
    );
    let lb = fn_body(&sched, "fnload_balance(&self)").expect("load_balance 不存在");
    assert!(
        lb.contains("ifbusiest_cpu<this_cpu{")
            && lb.contains("per_cpu_for(busiest_cpu).cfs_rq.lock();letmutdst_rq=per_cpu_for(this_cpu).cfs_rq.lock();")
            && lb.contains("per_cpu_for(this_cpu).cfs_rq.lock();letmutsrc_rq=per_cpu_for(busiest_cpu).cfs_rq.lock();"),
        "load_balance 的双持锁序未按 cpu index 升序 (反向取锁的两个搬动者即 AB-BA)"
    );
    // 偷取的账目动作只能在调用方建立的持锁临界区内发生.
    let steal = fn_body(&sched, "fnsteal_locked(").expect("steal_locked 不存在");
    assert!(
        !steal.contains("cfs_rq.lock()"),
        "steal_locked 自行加锁: 锁序前提被绕过"
    );
    assert!(
        steal.contains("src.dequeue(pid,weight,on_rq);letplaced=dst.enqueue(pid,vr,weight,false);"),
        "摘源 + 投目的未在同一段持锁期内闭合 (归属真空窗口复活)"
    );
}

#[test]
fn test_running_task_is_never_stolen() {
    let sched = prod_squashed(SCHED);
    // 在跑的任务"离树但记账", 搬走这份账 = 同一任务两核同时上核.
    for sig in ["fnplace_locked(", "fnsteal_locked("] {
        let body = fn_body(&sched, sig).unwrap_or_else(|| panic!("{sig} 不存在"));
        assert!(
            body.contains("current.load(Ordering::SeqCst)==pid"),
            "{sig} 缺少\"不搬正在核上跑的任务\"判据"
        );
    }
}

// ============================================================================
// 形状层: 不变式 5 (tick 体每窗口单名额)
// ============================================================================

#[test]
fn test_tick_body_uses_carry_cas_for_every_periodic_side_effect() {
    let sched = prod_squashed(SCHED);
    let tick = fn_body(&sched, "fntick(&self,cpu_id:usize)").expect("Scheduler::tick 不存在");
    assert!(
        !tick.contains("is_multiple_of("),
        "tick 体重新用 is_multiple_of 判周期: 派生 tick 跳值会整窗漏触发, 且各核同窗各自发起"
    );
    // kswapd / hotplug / reap / balance 四项各一次 CAS 名额.
    assert!(
        tick.matches("compare_exchange(").count() >= 4,
        "tick 体的每窗口名额不足 4 处 (carry+CAS 收口被削弱)"
    );
    for last in [
        "LAST_KSWAPD_TICK",
        "LAST_HOTPLUG_TICK",
        "LAST_REAP_TICK",
        "LAST_BALANCE_TICK",
    ] {
        assert!(
            tick.contains(&format!("{last}.load(Ordering::Acquire)")),
            "tick 体缺少 {last} 的 carry 读数"
        );
    }
}

// ============================================================================
// 负控制: 谓词必须在回归真的发生时亮起
// ============================================================================

#[test]
fn test_negative_control_predicates() {
    // 形状谓词逐个用合成回退样本验证: 把"被禁形态"喂回去, 断言仍能识别.
    // 抽 `Fixture` 别名避免 `&str` + `fn(&str) -> bool` 内嵌元组达 clippy `type_complexity` 阈值.
    type Fixture<'a> = (&'a str, &'a str, for<'b> fn(&'b str) -> bool);
    let fixtures: &[Fixture<'_>] = &[
        (
            "旧形态: 只持目的锁入队",
            "per_cpu_for(cpu_id).cfs_rq.lock().enqueue(pid,vr,weight,accounted);",
            |c| c.contains("cfs_rq.lock().enqueue("),
        ),
        (
            "旧形态: 按当前核撤账",
            "per_cpu().cfs_rq.lock().dequeue(pid,weight,true);",
            |c| c.contains("cfs_rq.lock().dequeue("),
        ),
        (
            "旧形态: 摘源后放锁再投目的",
            "{letmutsrc_rq=per_cpu_for(busiest_cpu).cfs_rq.lock();\
             src_rq.pick_next();src_rq.dequeue(pid,weight,true);\
             self.cfs_enqueue_to(pid,this_cpu);}",
            |c| {
                c.contains("src_rq.pick_next();src_rq.dequeue(pid,weight,true);")
                    && c.contains("self.cfs_enqueue_to(pid,this_cpu);")
            },
        ),
        (
            "旧形态: 清凭证不清归属",
            "PROCESS_TABLE.with_process(pid,|p|{p.cfs_on_rq.store(false,Ordering::Release);});",
            |c| {
                c.contains("p.cfs_on_rq.store(false,Ordering::Release);")
                    && !c.contains("claim.commit(NO_RQ_CPU)")
            },
        ),
        (
            "认领后早退不回滚 (PENDING 泄漏)",
            "fncfs_deactivate(&self,pid:Pid){letsome(mutclaim)=self.claim_rq(pid)else{\
             return;};ifclaim.prev_cpu==NO_RQ_CPU{return;}}",
            |c| {
                // 被禁形态: 凭证既未 commit 也未因函数结束而 Drop 到一个已知的
                // 安全值上 —— 文本上表现为"早退分支内既无 commit 也无回滚".
                c.contains("ifclaim.prev_cpu==NO_RQ_CPU{return;}")
                    && !c.contains("claim.commit(NO_RQ_CPU)")
            },
        ),
        (
            "唤醒前先置 Ready (丢自愈)",
            "pubfnunblock(&self,pid:Pid){if!need_enqueue{returnNone;}\
             let_set_state_safe(ProcessState::Ready);self.cfs_enqueue_to(pid,cpu);}",
            |c| match (
                c.find("self.cfs_enqueue_to("),
                c.find("set_state_safe(ProcessState::Ready)"),
            ) {
                (Some(a), Some(b)) => b < a,
                _ => false,
            },
        ),
        (
            "认领内等待 (AB-BA)",
            "fntry_claim_rq(&self,pid:Pid)->Option<u32>{\
             PROCESS_TABLE.with_process(pid,|p|{loop{ifp.cfs_rq_cpu.load(..)!=MIGRATE_PENDING{\
             returnSome(cur);}core::hint::spin_loop();}})}",
            |c| c.contains("loop{") && c.contains("spin_loop()"),
        ),
        (
            "锁序反向 (降序双持)",
            "letmutdst_rq=per_cpu_for(this_cpu).cfs_rq.lock();\
             letmutsrc_rq=per_cpu_for(busiest_cpu).cfs_rq.lock();\
             ifbusiest_cpu>this_cpu{steal_locked()}",
            |c| c.contains("ifbusiest_cpu>this_cpu{"),
        ),
        (
            "tick 体回到 is_multiple_of",
            "fntick(&self,cpu_id:usize){letnew_tick=get_ticks();\
             ifnew_tick.is_multiple_of(64){self.load_balance();}}",
            |c| c.contains("is_multiple_of("),
        ),
    ];

    for (name, fixture, predicate) in fixtures {
        let squashed = squash(&strip_comments(fixture));
        assert!(
            predicate(&squashed),
            "负控制失效: 未识别合成回退样本 {name:?} —— 对应门禁已形同虚设"
        );
    }

    // 反向: 修复形态不得被上述谓词命中.
    let fixed = squash(
        "fntry_claim_rq(&self,pid:Pid)->Option<u32>{\
         PROCESS_TABLE.with_process(pid,|p|{letcur=p.cfs_rq_cpu.load(Ordering::Acquire);\
         ifcur==MIGRATE_PENDING{returnNone;}\
         p.cfs_rq_cpu.compare_exchange(cur,MIGRATE_PENDING,Ordering::AcqRel,Ordering::Acquire)\
         .ok().map(|_|cur)}).flatten()}",
    );
    assert!(
        !(fixed.contains("loop{") && fixed.contains("spin_loop()")),
        "负控制失效: 非阻塞认领被误判为等待"
    );

    // 生产文件自身必须干净 (谓词对真源码返"无回退"才谈得上有意义).
    let sched = prod_squashed(SCHED);
    assert!(!sched.contains("cfs_rq.lock().enqueue("));
    assert!(!sched.contains("cfs_rq.lock().dequeue("));
    assert_eq!(
        sched.matches("cfs_rq_cpu.store(").count(),
        1,
        "真源码已存在多处归属直接写入: 单写入者契约已破"
    );
    let tick = fn_body(&sched, "fntick(&self,cpu_id:usize)").expect("Scheduler::tick 不存在");
    assert!(!tick.contains("is_multiple_of("));
}
