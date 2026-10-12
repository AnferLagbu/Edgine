# MM 帧所有权防线复跑取证：double free 签名在修复后各批次恒零

> 定位：本文是 P-15/P-16（用户页帧平行归还收口 + 帧所有权元数据）技术验收中「double free 消失」一项的**结果报告快照**（发布即冻结）。修复本体与决策已随 `f032b01d` 入库、验收记录见 `docs/plan/net-e2e-tcp-echo-flakiness.md` §4.6 与 §6 P-15/P-16；本文补齐的是 HANDOFF 类一登记的"复跑取证 + 落档"收口动作，不新增任何修复主张。

## 一、被验证的防线

帧所有权元数据（owner tag）是用户裁定**永久常驻**的观测面（非取证脚手架）：PMM 第 5 簿记区 `FRAME_OWNER` 按 pfn 记录每帧「分配点 + 最近归还点」各一枚 `Location` 指针，异常归还时在串口点名归因。`privileged/mm/pmm.rs::do_free` 的三条签名字符串即本轮扫描判据：

| 签名 | 触发条件 | 异常处置 |
|---|---|---|
| `[PMM] Warn: double free at pfn N (addr=…) alloc={}:{},first_retire={}:{},this_free={}:{}` | 归还时位图已为 0（帧已在空闲链） | 三元组告警 + **早退不入链**（二次入链会让链表成环） |
| `[PMM] Error: invalid page 0x…` | pfn 越出物理内存上界 | 告警 + 拒绝归还 |
| `[PMM] Warn: free before bitmap init at 0x…` | 簿记未就绪时归还 | 静默返回（不踩未初始化元数据） |

处置口径 = **告警 + 不归还**，不 panic、不静默 —— 用户裁定的既定前提，本快照复核其在生产路径的实际表现。

## 二、修复前基线与修复后各批次读数

修复前的现场（P-15 定案输入）：x86_64 e2e 存在两类失败形态（启动挂死于 KPTI/`destroy` 边界、首次回显全链成功后内核态 `#GP`），与 `[PMM] Warn: double free` 同源 —— 决定性三元组显示 `first_retire = this_free = privileged/mm/api.rs:81`（同一行双方争同一帧），根因是 proc 层按固定用户栈虚拟区间逐页 `pmm_free_page` 的平行归还绕过了 `destroy_page_table` 的计数持有权与 TLB 追平两道防线。旧格式告警（无三元组）的历史串口样本存于 `other/build/log/`（如 `BNDFAIL_run10.log` pfn 32302/32303、`smp1_fail_3.log` pfn 8655，属 owner tag 落地前的旧构建形态）；「修复前 1/6 轮出现」的正式口径以 plan §4.6 修复后首批判读表为准。

修复后累计 **32 轮** x86_64 取证批串口，三条签名扫描全部恒零：

| 批次 | 构建 | 轮数 | double free | invalid page | free-before-init | 备注 |
|---|---|---|---|---|---|---|
| §4.6 修复后首批 | `f032b01d` 后 | 6 | **0/6** | 0/6 | 0/6 | 同批启动里程碑 6/6、形态 A/B 归零 |
| P-21 验收批 ×2 | `ccdab0c9` | 10+10 | **0/20** | 0/20 | 0/20 | TCP echo 里程碑 20/20 |
| 本快照复跑批 | `6da3841b` | 6 | **0/6** | 0/6 | 0/6 | TCP 里程碑 6/6；残差 2 轮（r1/r6）均为 `[udp] RECVFROM=0` = P-24，MM 面零命中 |

复跑批原始串口存档于 `other/mm-verify-evidence/`（本地留证，`other/` 在 gitignore 内）：`qemu_e2e_tcp_x86_64.log` / `.fail1.log` / `.fail6.log` / `qemu_boot_x86_64.log` / `mmverify_e2e.log`。

## 三、回归锁构成（36 例 host-tests，全绿）

防线由四份 host-tests 契约文件锁死（判据是源文本契约 + 逻辑单例复演，修改实现绕过任一条即红）：

- `pmm_frame_ownership_test.rs`（13 例）：锁死六条不变式 —— FRAME_OWNER 为 `init_bitmap` 第 5 区且被位图预留、归因写入点唯一且粒度正确（退役点必须记在入延迟释放链的时刻）、`#[track_caller]` 穿透到 `*_phys` 包装、异常归还早退不入链、观测面软取单例不打挂控制流、`RawMetaStore`/`VecMetaStore` 双载体同构。
- `user_proc_frame_release_test.rs`（9 例）：proc 层归还必须走 `mm::{pmm_alloc_page_phys, pmm_free_page_phys,…}` 权威入口，`destroy`/`replace_user_space` 无平行用户栈归还，ELF 回滚先置 Zombie 再走 `remove_and_free` 单源（含"缺 Zombie 标记"与"就地手抄归还"两个负控制样本）。
- `mm_deferred_free_contract_test.rs`（10 例）：延迟释放链的归因与入链纪律。
- `user_proc_reclaim_contract_test.rs`（4 例）：进程回收面单一销毁源。

## 四、边界与残余

- owner tag 是**观测 + 遏制**面：它让异常归还可点名、拒绝二次入链，但不替代"消灭平行实现"本身 —— 后者已由 P-15 的权威归还单源完成，两者是同一工程的两道工序。
- 本复跑批的 2 轮失败与本防线无关（形态 = UDP 出站首包从未离 NIC，pcap 定性见 P-24 台账），MM 面签名恒零的结论不受其影响。
- 扫描判据取串口签名字符串而非事后断言，因此对未来批次同样可复用：任何 e2e/长稳批跑完后 `grep "double free at pfn\|Error: invalid page\|free before bitmap"` 即得同口径读数。
