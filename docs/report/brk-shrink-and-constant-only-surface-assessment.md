# brk 收缩分支与「只有常量无实现」面评估

> 结论：两处问题共享同一形态——**控制流或标志位声明了某个能力，但实现路径不存在**。`set_brk` 的收缩分支在「新 brk 落在 start_brk 与 current_brk 之间」时不可达，会构造 `start > end` 的倒置 VMA 且不拆页表；`MAP_FIXED` 只有常量、从不参与判别，调用方显式要求固定地址时被当作 hint 静默忽略。此外有 11 条 syscall 编号只有常量无分发臂，SGEG 私有编号仍侵入 Linux 已分配区。

**快照与基线**：HEAD `a7b6d87b`（2026-11-13 20:26 +0800），取证日期 2026-11-14。全程只读。

## 一、brk 收缩分支

### 1.1 实现

`privileged/mm/vma.rs:1009-1028` 的 `set_brk(new_brk, cr3)`：

```rust
let page_aligned = (new_brk + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);
let start_brk = self.start_brk.load(Ordering::Acquire);
let current_brk = self.brk.load(Ordering::Acquire);

if page_aligned > start_brk {
    let vma = Vma::new(current_brk, page_aligned, flags, VmaType::Heap);
    self.insert_vma(vma)?;
    self.brk.store(page_aligned, Ordering::Release);
} else if page_aligned < current_brk {
    self.brk.store(page_aligned, Ordering::Release);
    self.remove_range(page_aligned, current_brk, cr3);
}
```

判据顺序是关键：第一个分支比较的是 `page_aligned` 与 **`start_brk`**，不是与 `current_brk`。

### 1.2 不可达区间推演

取一个具体状态：`start_brk = 0x1000`、`current_brk = 0x5000`，调用 `brk(0x3000)`（收缩 8 KiB）：

1. `page_aligned = 0x3000`；
2. `0x3000 > 0x1000` 为真 → 进入**扩展**分支；
3. 构造 `Vma::new(0x5000, 0x3000, …)`，即 `start = 0x5000 > end = 0x3000`；
4. `insert_vma` 在 `Vma::new` 处没有区间合法性检查（`vma.rs:220-232` 的构造函数直接写入字段），故倒置 VMA 被收入 `Vec<Vma>`；
5. `self.brk.store(0x3000)` 把 brk 下调；
6. 页表一页都没拆——`remove_range` 未被调用。

第二个分支 `else if page_aligned < current_brk` 只在 `page_aligned <= start_brk` 时可达，即「缩到 start_brk 及以下」这一种情形；「缩到 start_brk 与 current_brk 之间」这一最常见的中段收缩，永远走不到它。

### 1.3 影响

- **页表泄漏**：堆缩减后虚拟页仍映射，物理页不归还；反复「扩-缩」同一区间会持续占用内存，且 `brk` 值与 VMA 表不再一致。
- **VMA 不变量破损**：`Vec<Vma>` 中出现 `start > end` 的条目。后续 `find_vma` 按区间查找时，倒置条目的行为取决于实现（当区间判据为 `start <= addr && addr < end` 时永不命中，等同于该 VMA 隐形），但 `remove_range` / `insert_vma` 的排序与切分逻辑会遇到不满足序关系的输入。
- **不可达路径无测试**：`host-tests` 中没有针对 brk 收缩的回归用例；`set_brk` 唯一调用者是 `functions/syscall/brk.rs:47`，该入口不含区间前置校验。

### 1.4 与「正确实现」的差距

正确判据应是三路：

- `page_aligned > current_brk` → 扩展，插入 `[current_brk, page_aligned)`；
- `page_aligned < current_brk` → 收缩，`remove_range(page_aligned, current_brk)` 并拆分/截断 VMA；
- 相等 → 空操作。

当前实现用 `start_brk` 当分界，把「扩展」与「中段收缩」混为一谈。这不是「边界少判一个」，而是**两条路径的判据取错了量**。

## 二、`MAP_FIXED`：常量存在但从不参与判别

`functions/mm/mmap.rs:34`：

```rust
pub const MAP_FIXED: i32 = 0x10;
```

全仓检索 `MAP_FIXED`，除该定义外没有任何读取点。`mmap` 实现只消费 `MAP_SHARED`（`:28`）、`MAP_PRIVATE`（`:30`）、`MAP_ANONYMOUS`（`:32`），并在 `:114` 读取 `MAP_PRIVATE` 位。

后果：用户传入 `MAP_FIXED` 时，内核**不报错、不拒绝、也不遵守**——按普通 hint 语义挑一个地址返回。调用方（尤其是自举分配器、JIT、共享内存映射器）拿到一个「成功但地址不是我要的」结果。这是标志位层面的假成功，与 syscall 臂层面的假成功同构。

作为对照，`MREMAP_FIXED` 的处置是正确形态：`functions/mm/mremap.rs:16` 说明「不支持（由 glibc 在用户态模拟）」，`:128` 处显式拒绝并注明「未实装，必须拒绝而不是静默忽略」，`privileged/mm/vma.rs:897-917` 对含未实现位的 flags 返回 `EINVAL`。同一份代码里同时存在「显式拒绝」和「静默忽略」两种处理，说明后者不是设计选择，而是遗漏。

## 三、11 条只有常量无分发臂的 syscall

对 `privileged/syscall/types.rs` 的常量名与两侧 dispatcher 的 token 取差集：

| 常量 | 编号 | 常见调用方 |
|---|---|---|
| `SYS_PREAD64` | 17 | 任何执行 `pread` 的程序 |
| `SYS_PWRITE64` | 18 | 任何执行 `pwrite` 的程序 |
| `SYS_PSELECT6` | 270 | glibc `select` / `pselect` |
| `SYS_ACCEPT4` | 288 | 服务端 socket 常规路径 |
| `SYS_RENAMEAT2` | 316 | coreutils `mv`、原子替换 |
| `SYS_PREADV2` | 327 | 带 `RWF_*` 标志的向量 I/O |
| `SYS_PWRITEV2` | 328 | 同上 |
| `SYS_OPENAT2` | 437 | 新式路径解析（`RESOLVE_*`） |
| `SYS_FACCESSAT2` | 439 | 带 flags 的 `faccessat` |
| `SYS_EPOLL_PWAIT2` | 441 | 带超时的 epoll |
| `SYS_FCHMODAT2` | 452 | 带 flags 的 `fchmodat` |

这批编号全部落在 Linux 已分配区，且 `17`/`18` 是文件 I/O 基础面。编号存在但不分发，意味着任何按 Linux 头文件编译的程序都可能调用到无臂编号，得到分发兜底的错误而非明确契约。

其中 `SYS_PWRITE64` 与 `SYS_ACCEPT4` 在全仓另有出现，但位置是 `privileged/proc/domain.rs:28-69` 的**权限判定表**与 `:239` / `:263` 的单测断言，仍无任何分发臂。这使问题更尖锐：这两个编号会先经过域权限裁决（可能返回 `EPERM`），随后落进无臂兜底，调用方得到的是一个看似经过鉴权的错误。

需要说明的是：`audit_unwired_pub_fn.py` 报 R2 = 0，但其收集面只覆盖 36 个编号（脚本自身输出），与 `types.rs` 中可解析的 247 条常量不匹配。R2 = 0 不能作为「无未接线编号」的证据。

## 四、SGEG 私有编号侵入 Linux 已分配区

`privileged/syscall/types.rs`：

- `:611` `SYS_SGEG_LOGIN = 400`
- `:657` `SYS_SGEG_DISK_INSTALL = 453`
- `:677` `SYS_SGEG_HOTPLUG_STATUS = 463`

按 Linux x86_64 官方表，453–463 已被占用：453 `map_shadow_stack`、454 `futex_wake`、455 `futex_wait`、456 `futex_requeue`、457 `statmount`、458 `listmount`、459 `lsm_get_self_attr`、460 `lsm_set_self_attr`、461 `lsm_list_modules`、462 `mseal`、463 `setxattrat`。其中 `mseal(462)` 会直接落入 `SYS_SGEG_REBOOT` 的分发臂。

这不是「未来可能冲突」而是**当下已经冲突**：任何使用新式 Linux ABI 的静态程序，只要调用 `mseal` 或 `statmount`，就会触发内核的私有语义（重启、磁盘安装等）。与「只有常量无实现」相反，这里是「有实现且语义完全不同」，危险性更高。

## 五、共同形态与检测难点

三处问题的共同点是：**能力在声明层存在，在实现层缺失，且缺失不产生错误信号**。

| 位置 | 声明 | 缺失 | 观测到的行为 |
|---|---|---|---|
| `set_brk` | 收缩分支 | 中段收缩不可达 | 返回新 brk 值，页未拆 |
| `MAP_FIXED` | 标志常量 | 无判别 | 返回成功，地址非请求值 |
| 11 条 syscall | 编号常量 | 无分发臂 | 返回兜底错误，非 ENOSYS 契约 |
| SGEG 编号 | 私有语义 | 与 Linux 撞号 | 返回错误语义 |

四者都不会被现有门禁抓住：F9 只查死代码抑制，不查「常量是否有实现」；`audit_unwired_pub_fn.py` 的 R2 收集面窄于常量全集；没有任何腿检查「标志位是否被读取」。

## 六、对计划的意义

这四类的处置粒度不同，不应合并：

- `set_brk` 是**正确性缺陷**，修法是判据换量（以 `current_brk` 分界）+ 构造函数加区间不变量 + 一条「扩-缩-再扩」的行为回归；
- `MAP_FIXED` 是**契约缺口**，要么实装（含重叠、对齐、权限校验），要么像 `MREMAP_FIXED` 一样显式 `EINVAL`，不能继续静默忽略；
- 11 条编号是**接线缺口**，可按「第三方程序触达概率」排序补齐，并把 R2 的收集面扩到 `types.rs` 全量后重新取基线；
- SGEG 撞号是**ABI 污染**，必须先于外部程序试点处置，否则试点会在 `mseal` 上得到难以归因的错误。
