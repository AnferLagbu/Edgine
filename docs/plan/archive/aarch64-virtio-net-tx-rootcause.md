# P-31: aarch64 virtio-net TX 超时根因与 legacy vring 布局修复

> 输入依据：[net-feature-alignment.md](./net-feature-alignment.md) T1 档位2 软观测首轮数据
> （`observe=fail listen=0 addr=1 tx_timeout=11`）与 2026-10-19 复现取证日志
> （`/tmp/a64_tx_repro.log` 实录：`version=1, legacy mode` / `vq0 max_size=1024` / 首包即 `TX 超时`）。

---

## 背景

net-feature-alignment T1 软观测坐实：aarch64 QEMU virt 上 virtio-net 数据面从未真实工作 —
DHCP 首发（70B discover）TX 即超时、全部发送超时、地址只能落 fallback 静态、用户态 echo 链路
不可达。x86_64 主路径走 e1000（不经 virtio-mmio），故双架构门槛长期全绿掩盖了该缺陷。

## 根因（已坐实，非猜测）

QEMU `-device virtio-net-device` 在 virt 机器上以 **legacy 传输（Version 寄存器 = 1，
0.9.5 协议）** 呈现。legacy 协议**没有 QueueNum 协商寄存器**：设备端队列尺寸在 realize 时
固定为 1024（即驱动日志读到的 `QueueNumMax=1024`），且设备从 `QueuePFN` 收到基址后按
0.9.5 规范**自行推算**三段布局：

```
desc  = pfn << 12
avail = desc + 16 * num          // num = 1024 → desc + 0x4000
used  = align(avail + 6 + 2*num + 2, 4096)   // → desc + 0x5000
```

而驱动侧 `VirtQueue::new` 以编译期固定 `VQ_SIZE = 32` 布置内存
（desc 表仅 512B、avail@0x200、used@0x1000，共 2 页）。**设备按 num=1024 推算的
avail/used 地址（desc+0x4000 / desc+0x5000）落在驱动分配区之外**：设备读到恒 0 的
`avail.idx` → 永不取描述符、`used` 写向别处 → 驱动轮询 `pop_used` 永远空转 →
100k spin 后 `TX 超时`。RX 预填同理失效（设备看不到任何可用缓冲）。首包即超时、
与包内容无关，与观测完全吻合。

同批连带发现并修复的 modern 路径寄存器 bug（本设备 legacy 未走到，modern 对照实验坐实）：

1. modern 路径 **QueueNum（0x038）从未写入**：规范要求驱动写队列尺寸；QEMU 仅在
   QueueReady=1 时以 QueueNum 初始化 vring，未写则尺寸不定 → modern 路径整链不通。
2. `QUEUE_READY` 规范偏移为 **0x044**。取证中途曾误按「与 legacy QueuePFN 同址
   0x040」改写，modern 对照实验（QEMU guest_errors 实录
   `write to legacy register (0x40) in non-legacy mode`）证伪后恢复 0x044 —
   **教训：寄存器偏移必须以规范/设备端行为实证，不得靠推记**（已锁进契约测试
   `mmio_queue_ready_offset_is_0x044`）。

## 根因链补全（R6 实机取证二轮：legacy 修完 L1 后仍不通，逐层再剖）

- **L2 GuestPageSize**：QEMU `VIRTIO_MMIO_QUEUE_PFN` 按 `value << ctz32(GUEST_PAGE_SIZE)`
  解析基址，该寄存器（0x028）复位默认 0 → PFN 被误读成字节地址。修复：legacy 下
  FEATURES_OK 后、首写 PFN 前写 `set_guest_page_size(4096)`。
- **L3a legacy 队列激活范式序**：仅写 PFN 不触发 QEMU 设备端 vring 布局刷新
  （`virtio_queue_update_rings` 需 num/desc/align 齐备时机）。修复：按 Linux
  legacy-mmio 驱动范式序 `QueueNum(回写设备上限) → QueueAlign(0x03c, 4096) → QueuePFN`；
  legacy 无 QueueReady（0x040 即 PFN，绝不可覆写）。修复后设备开始消费（used_idx 递增）。
- **L3b legacy virtio_net_hdr 尺寸**：0.9.5 未协商 MRG_RXBUF 时头恒 **10 字节**（无
  num_buffers），驱动写 12 → 设备把帧起点前移 2 字节 → 以太网头损坏 → slirp 丢弃 →
  DHCP 无响应。修复：`LEGACY_NET_HDR_SIZE = 10`，legacy 协商分支按 10 布置。
- **取证判别实验**（一次性排除「双重分配覆写」与「VA→PA 映射非线性」两假说）：
  avail 页 padding 间隙写哨兵 u64 + QEMU `pmemsave` 全 RAM dump 搜索 → 哨兵精确落位、
  RX avail 环完美、used 全 0 → 证明驱动侧内存布局正确，故障在设备侧交互协议层。

## 目标

- legacy 传输下 virtio-net 数据面真实工作：aarch64 QEMU DHCP 拿到租约（`addr=dhcp`）、
  TX 超时计数归零、T1 软观测翻绿（`observe=pass`）。
- vring 布局改为**运行时 num**（与设备端推算一致），modern/legacy 双形态均正确。
- x86_64 e1000 路径零影响（不共码）。

## 任务条目

- **R1: queue.rs 运行时 vring 布局**
  - 描述：`VirtQueue` 从编译期 `VQ_SIZE=32` 定长数组改为运行时 `num` 字段驱动的布局；
    `VirtQueue::new(legacy: bool, num: u16)`；avail/used 环元素访问改指针偏移 + `% num`。
  - 方案：布局函数按规范双形态计算 — legacy: `avail=16*num`、`used=align(avail+6+2*num+2, 4096)`；
    modern: `avail=16*num`、`used=align(avail+6+2*num+2, 4)`、used 尾保留 `avail_event`(u32) 4 字节
    （EVENT_IDX 虽未协商，尺寸预留防越界写）。`submit`/`pop_used`/描述符链全部以 `self.num` 取模。
    布局计算抽为纯函数 `vring_layout(legacy, num) -> (avail_off, used_off, total)` 以便 host 单测。
  - 状态：[X]
  - 详情：内存开销 — legacy num=1024 时全布局 `vring_layout` 实测 total=0x8000 即
    **8 页**（≈32KB/队列；desc 16KB + avail 2KB + used 16KB 取整页），net 2 队列 +64KB；
    512MB QEMU 机可接受，不引入自造 cap（legacy 布局必须与设备推算全量一致，截断即再次错位）。
    实施注记：avail 偏移基公式为 `desc + 16*num`（+6 属 avail 环体内部 used 推算），
    实现与 QEMU `virtio_vring` 推算逐字节验证一致（契约测试 `legacy_1024_matches_device_inference`）。

- **R2: mod.rs modern 寄存器修正**
  - 描述：新增 `QUEUE_NUM`(0x038) 与 `set_queue_num()`；`QUEUE_READY` 保持规范
    0x044（取证中途误改 0x040 已回退，见上节教训）；新增 `QUEUE_ALIGN`(0x03c) 与
    `set_queue_align()`（legacy 范式序用）；`setup_queue_addrs` 序改为
    QueueNum → 地址三段 → QueueReady=1。
  - 方案：修正常量并在 `setup_queue` 调用链补写 num；头注释寄存器表已正确（0x038/0x040），仅改代码。
  - 状态：[X]

- **R3: net.rs 队列尺寸接线**
  - 描述：驱动按传输形态选择 num — legacy: 全量取 `QueueNumMax`（与设备端一致）；
    modern: 32（写 QueueNum 协商）。`VirtQueue` 分配时机移到读到 `QueueNumMax` 之后。
  - 方案：`new()` 中先 `select_queue(i)+queue_num_max()` 再分配 rx/tx vq；
    队列 legacy 判定统一改用**传输层** `device.is_legacy()`（现混用特性位 `!has_v1`，
    本设备恰好一致但语义应锚定传输寄存器形态）；`setup_queue` 按 R2 补 num 写入。
    `rx_buffers` 表维持 32 槽（desc_idx≥32 时既有无效槽分支回收，不越界）。
    实装追加（根因链 L2/L3a/L3b）：legacy 分支 setup_queue 按范式序写
    num→align→PFN；Step 5.5 legacy 写 GuestPageSize(4096)；legacy 头尺寸取
    `LEGACY_NET_HDR_SIZE=10`。
  - 状态：[X]

- **R4: blk.rs 调用点适配**
  - 描述：`VirtQueue::new(is_legacy)?` 签名变更的编译强制适配。
  - 方案：blk 维持 modern 语义传 num=32（QEMU boot 场景无 legacy virtio-blk 设备；
    legacy blk 布局待有真实消费者时按 R1 布局函数自然可得）。加 SIMPLIFIED 标注。
  - 状态：[X]

- **R5: 回归测试**
  - 描述：锁死布局计算与寄存器偏移，防回归。
  - 方案：queue.rs 内 `#[cfg(test)]` 纯函数用例（legacy num=1024 → avail@0x4000 /
    used@0x5000；modern num=32 偏移 4 对齐；total 容纳全段）进 `make test-kernel-host`；
    host-tests 新增 `virtio_vring_layout_contract_test.rs` 契约（10 用例：文本锁
    `QUEUE_READY: usize = 0x044`、`QUEUE_NUM` 存在、legacy 全量取 num_max、
    GuestPageSize 先于 PFN、legacy 范式序 num→align→pfn、legacy 头 10 字节、
    modern 写 num 先于地址与 ready）。
  - 状态：[X]

- **R6: 实机验证 + 软观测翻绿**
  - 描述：aarch64 QEMU 实测 DHCP 租约 + `a64_e2e_observe` 数据面指标达标；x86 腿回归不破坏。
  - 方案：`./scripts/qemu_boot_test.sh aarch64`（软观测数据行 `observe=pass addr=dhcp tx_timeout=0`）
    + x86_64 腿全里程碑；T1 台账（net-feature-alignment.md）回填翻绿数据。
  - 状态：[X]
  - 实测（2026-10-19）：legacy（生产默认形态）`DHCP configured (lease applied)` +
    IPv6 SLAAC + **TX 超时/掉帧 = 0**；`-global virtio-mmio.force-legacy=false` modern
    对照同样 `lease acquired`。正式 e2e 数据行
    `observe=fail reason=not_ready listen=0 addr=1 tx_timeout=0` — **P-31 目标
    （tx_timeout 归零 / DHCP 租约）达成**；`listen=0` 归因于**与本缺陷无关的
    预存问题**（见 R7），软观测按裁定不阻断。

- **R7: 移交项（非 P-31 根因链，待用户裁定处置）**
  - 描述：aarch64 EL0 用户程序启动即 SYNC — `ESR=0x92000006`（EC=0x24，trapped
    MSR/MRS/System register 访问）、`ELR=0x4000D8`（user 低区）杀死 init → e2e
    `listen=0`（`[tcp] Listening` 缺失）；modern 跑还见过 EC=0x0E（Illegal exception
    return）。x86 e1000 路径无此问题。属 AGENTS §12.5 工程外预存问题，未擅自处置。
  - 方案：用户裁定（立独立工程 / 记 plan / 搁置）后单开。
  - 状态：[X]
  - 详情：用户 2026-10-19 裁定立独立根因工程 → 已立项 [P-32](./aarch64-el0-sysreg-sync-rootcause.md)，
    取证数据与候选根因已移交其 E1/E3；本 plan 不再跟踪。
    勘误（P-32 E2 后续证实）：本条「杀死 init」归因不成立 — EL0 SYNC 实为 KPTI-09
    探针预期行为，init 父进程存活；`listen=0` 直接原因是用户态 echo 服务的架构门禁
    （已随 P-32 E3① 放行），另现真预存 flaky 竞态由 P-32 继续攻坚。

## 验证门槛

AGENTS.md §2.3 全部 6 条 + R5/R6 专项。诊断 feature 先例不适用于本轮
（根因已由日志直接坐实，无需埋点）。

## 关联文档

- [net-feature-alignment.md](./net-feature-alignment.md) — T1 软观测（本工程输入）
- [src/kernel/privileged/driver/virtio/queue.rs](../../../src/kernel/privileged/driver/virtio/queue.rs) — 布局重构主体
- [src/kernel/privileged/driver/virtio/mod.rs](../../../src/kernel/privileged/driver/virtio/mod.rs) — 传输寄存器修正
- [src/kernel/functions/driver/virtio/net.rs](../../../src/kernel/functions/driver/virtio/net.rs) — 队列尺寸接线
