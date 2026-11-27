# 地址空间生命周期（Edgine 现状）

> 面向维护 `privileged/` 的开发者与排查 `fork`/`exec`/退出泄漏的人：说明一个用户地址空间从装载到回收由谁持有、谁注销，以及 mm 与页表两套计数为什么必须分层。工程口径见 [mm-lifecycle-project.md](../plan/mm-lifecycle-project.md) 的 M5，cr3 侧先例见 [cr3-lifetime-ownership.md](../plan/archive/cr3-lifetime-ownership.md)。

## 一条地址空间的两半

| 半边 | 载体 | 计数 | 归零动作 |
|---|---|---|---|
| 页表 | `Process.cr3`（x86 `CR3` / aarch64 `TTBR0`） | PMM 帧持有计数 `frame_inc` / `frame_dec` | `frame_dec` 归零才 `vmm_destroy_page_table`，按 leaf 归还数据页 |
| 描述符 | `Process.mm: AtomicPtr<MmStruct>` | `MmStruct.ref_count`（`mm_retain` / `mm_release`） | `mm_release` 归零才 `Box::from_raw` 回收 VMA 容器与 brk/mlock 元数据 |

二者不是一一对应：`fork` 走 COW **克隆出一棵新页表**（新 cr3、新帧计数），但 VMA 描述符**不复制**，父子仍指向同一个 `MmStruct`（M2 裁定 B）。因此不能用 cr3 计数代替 mm 计数——否则父退出时会拆掉子进程仍在用的 VMA 列表。

## 四个时点

1. **装载**：`load_elf_from_memory_with_interp` 建 `MmStruct`（`ref_count = 1`，创建者引用），按 PT_LOAD 建 VMA，写 `start_brk` / `brk` / `mmap_base`，最后 `Box::into_raw(mm)` 交给 `Process.mm`；失败时局部 `Box` 自行回收，不重复归还。
2. **继承**：`fork` 与 `CLONE_VM` 共用 `Process::inherit_mm_from` —— 读父槽位、写子槽位、`mm_retain`。functions 子树禁止 unsafe，裸指针与计数登记都收在 privileged 层。
3. **换表（exec）**：临时进程装载新 mm 后，`replace_user_space` 把新 cr3/mm 交给当前 PID；旧 mm 的这份引用在此注销。新 mm 是**所有权转移**（临时进程 → 当前进程），源槽位只清空、不 release。
4. **退出**：`Process::drop` 先按 `frame_dec(cr3)` 归零门销毁页表，再 `mm_release` 注销本进程那份 mm 引用；最后一个持有者归零时回收 VMA 容器。

## 为什么 `get_current_mm` 可以返回 `&'static`

缺页与 VMA 系统调用经当前任务的 `Process.mm` 取 mm。该借用成立的依据是：**当前任务自身持有一份 mm 引用**，只要任务还在运行，计数就不会归零，对象不会被 `mm_release` 回收。`fork` 出的子进程与 `CLONE_VM` 线程各自持有独立引用，先退出的一方只减计数。

## 边界（不在本层解决）

- **VMA 列表仍跨 fork 共享**：子进程 `mmap`/`brk` 会改动父进程看到的描述符。这是 M2「B 相对完整」的既定语义，M5 只补所有权注销，不改共享策略；要改成 Linux `dup_mm` 语义需另立工程（含 COW 化的 VMA 容器）。
- **没有 per-mm 激活集合**：mm 释放只保证无 Process 持有引用，不表达「哪些 CPU 仍驻留在该地址空间」；后者由 cr3 销毁路径的 TLB 协议与 per-mm 激活集合议题分别承担（见 [tlb-shootdown-epoch.md](../plan/archive/tlb-shootdown-epoch.md)）。
- **页面内容归页表**：`mm_release` 只回收描述符，不归还任何数据页；页帧由 `destroy_page_table` 的 leaf 遍历与帧计数负责。
