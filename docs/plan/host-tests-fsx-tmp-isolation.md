# host-tests fsx 用例 /tmp 目录隔离缺陷

> 0-1 句话说清"为什么有这个计划": `fsx` 系列用例使用固定绝对路径 `/tmp/edgine-fsx*`（无 PID/随机后缀），并发运行或异常中断后残留状态会污染后续运行，产生数据完整性假失败，进而污染 §2.3 门槛 1/4 的判定。

## 工程计划 A: 测试目录隔离

### 背景

- **固定目录名（并发 + 残留双向污染）**
  - 描述: [fsx.rs](file:///home/anfer/Code/Edgine/host-tests/src/fsx.rs#L59) 默认 `test_dir = /tmp/edgine-fsx`，[fsx_integration_test.rs](file:///home/anfer/Code/Edgine/host-tests/tests/fsx_integration_test.rs) 的 6 个用例分别使用 `/tmp/edgine-fsx-quick` / `-tmpfs` / `-ext2` / `-exfat` / `-overlayfs` / `-stress`，**全部为固定名**（无 `std::process::id()` / 随机后缀）。同仓库 [td25_comment_language_test.rs](file:///home/anfer/Code/Edgine/host-tests/tests/td25_comment_language_test.rs#L33-L35) 已采用 `temp_dir().join(format!("...-{}", std::process::id()))` 的正确范式，说明该缺陷可直接按既有范式收敛。放大因素有二：`run()` 入口只做 `create_dir_all`、**不清空既有内容**（[fsx.rs:122](file:///home/anfer/Code/Edgine/host-tests/src/fsx.rs#L122)）；`cleanup()` 直接 `remove_dir_all` 且不校验目录归属（[fsx.rs:349](file:///home/anfer/Code/Edgine/host-tests/src/fsx.rs#L349)），异常中断（Ctrl-C / 超时被杀）即留下残留。
  - 方案: 各用例与默认配置的 `test_dir` 改为 `std::env::temp_dir().join(format!("edgine-fsx-<name>-{}", std::process::id()))`（沿用 td25 范式）；或在 `run()` 入口改为「先清空再创建」以保证幂等。二者可并行实施，推荐前者为主（根治并发污染）、后者为辅（根治残留污染）。
  - 状态: [X]
  - 详情: 主辅两条**均**已实施。主：新增公开 helper `fsx::isolated_test_dir(name)`（`temp_dir().join(format!("edgine-fsx-{}-{}", name, process::id()))`），`FsxConfig::default()` 与 6 个集成用例 + 2 个 in-crate 用例全部改用该 helper。辅：`FsxFs::run()` 入口在 `create_dir_all` 前加 `let _ = fs::remove_dir_all(&self.config.test_dir);`，使「空目录」初始假设对残留免疫。附带收益：`cleanup()` 的 `remove_dir_all` 不再可能误删他方目录 —— 目录已按进程独占，归属校验由构造方式保证，无需额外改动。

- **实测证据（假失败）**
  - 描述: 2026-09-25 G-07 收尾轮验收时，另一并发会话（另一 IDE 终端）在同一仓库执行 `cargo test --test fsx_integration_test -- --test-threads=1`，与本轮 `./ci/build.sh all` / `make test-host` 触发的 fsx 用例共用同一批 `/tmp/edgine-fsx-*` 目录，产生数据完整性假失败（`disk_len` 与期望长度不符 —— 模型只跟踪自身写入，实际文件被另一写入方追加，与「固定目录 + 初始空状态假设」缺失一致）；同期 `rm -rf /tmp/edgine-fsx-*` 报「目录非空」，印证存在并发写入。对端进程结束后清理 `/tmp` 重跑：6/6 全过，`./ci/build.sh all` 连续两次 `Passed: 5 Failed: 0`，`make test-host` 106 个 `test result: ok`。
  - 方案: 修复轮按上一方案改造后，以「两进程并发跑同一用例」做反向验证（改造前应能复现假失败，改造后应稳定通过）。
  - 状态: [X]
  - 详情: 已按方案执行反向验证。并发启动 lib 单测二进制（`test_fsx_stress`）与集成测试二进制（同名 `test_fsx_stress`）—— 这正是改造前自撞的用例组合 —— 两进程均 `rc=0`，运行后 `/tmp/edgine-fsx-*` 无残留。另 `cargo test --release --test fsx_integration_test` 6 passed；`cargo check --all-targets` 0 warning；`cargo clippy --all-targets --release --keep-going -- -D warnings` rc=0。

- **影响面与边界**
  - 描述: 仅影响 host-tests 自身（`fsx` 是宿主侧 `std::fs` 压力工具，非内核逻辑，不涉 framework/services 正确性）；但会污染 AGENTS §2.3 门槛 1（`./ci/build.sh all` 含 host 测试）与门槛 4（`make test-host`）的判定，使真实失败与假失败无法区分，违背 §12.4「先定义成功标准再验证」的目标驱动前提。
  - 方案: 修复仅需改动测试侧目录构造，不需要 CI 新增步骤，也不改变任何被测量行为。
  - 状态: [X]
  - 详情: 实际改动面与预估一致 —— 仅 `host-tests/src/fsx.rs`（新增 helper、默认配置、`run()` 幂等化）与 `host-tests/tests/fsx_integration_test.rs`（6 处目录构造、导入调整）。未涉内核侧代码、未新增 CI 步骤、未改变任何被测量行为。

### 范围

- **不在本任务内**
  - 描述: `fsx` 压力模型的算法与校验逻辑；内核侧任何文件系统代码；CI 流程改动。
  - 方案: 仅动 `host-tests/tests/fsx_integration_test.rs` 与 `host-tests/src/fsx.rs` 的测试目录构造（及可选的 `run()` 幂等化）。
  - 状态: [X]
  - 详情: 范围守持 —— 未触碰 `fsx` 压力模型的算法与校验逻辑、内核侧文件系统代码、CI 流程。