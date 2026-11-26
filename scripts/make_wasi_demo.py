#!/usr/bin/env python3
"""再生成 P1 wasmi stdio demo 载荷 `scripts/fixtures/hello_wasi.wasm`.

该载荷是 wasi-sdk 编译的真实工具链 wasm 模块 (printf hello), 被
`scripts/make_initramfs_cpio.py` 打包为 `bin/hello_wasi.wasm`, init 的 `[WASMI]`
探针 fork + exec 它; 内核 `exec.rs` 按魔数分派装载用户态 wasmi 运行时 `wasmrun`,
由 wasmi 解释执行并经最小 WASI host 层把
`fd_write` 翻译到 Edgine `write` syscall, stdio 输出 "hello from wasmi".

为什么 committed fixture 而非构建期编译:
1. **无工具链依赖** —— CI / 干净环境不必装 wasi-sdk (类比 make_probe_wasm.py
   手写编码避免 wasm 工具链依赖); fixture 是预编译产物, 构建期仅复制.
2. **真实工具链 provenance** —— de-risk 价值在于「wasi-sdk 真产物能在用户态
   wasmi 下跑通」, 手写模块证不到生态兼容性. 源 `hello_wasi.c` 与编译命令
   一并入库, 升级 wasi-sdk 时用本脚本重生成并同批提交.

载荷取最小 WASI 子集 (fd_write + proc_exit), 与 P1「最小 WASI de-risk」口径一致.
P4 后 exec 派发只读 `/bin/wasmrun` (唯一 wasm 运行时, 见 make_initramfs_cpio.py),
stdio 输出必出自 wasmi (非载荷不兼容). 载体腿的 `[WASM]` 探针仍用 36 字节
`hello.wasm` (见 make_probe_wasm.py), 不消费本载荷.

用法:
    WASI_SDK_PATH=~/wasi-sdk/wasi-sdk-34.0-x86_64-linux \
        python3 scripts/make_wasi_demo.py --out scripts/fixtures/hello_wasi.wasm

退出码: 0 = 写出成功; 1 = 参数/工具链/IO 失败.
"""

from __future__ import annotations

import argparse
import os
import subprocess
import sys
from pathlib import Path

PROJECT_ROOT = Path(__file__).resolve().parent.parent
SOURCE = PROJECT_ROOT / "scripts" / "fixtures" / "hello_wasi.c"
# 钉住生成 fixture 的 wasi-sdk 版本 (升级须重生成 fixture 并同批提交).
WASI_SDK_VERSION = "wasi-sdk-34.0"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", type=Path, required=True, help="输出 .wasm 路径")
    parser.add_argument(
        "--sdk",
        default=os.environ.get("WASI_SDK_PATH", ""),
        help="wasi-sdk 根目录 (默认取 WASI_SDK_PATH 环境变量)",
    )
    args = parser.parse_args()

    sdk = Path(args.sdk).expanduser()
    clang = sdk / "bin" / "clang"
    sysroot = sdk / "share" / "wasi-sysroot"
    if not clang.is_file() or not sysroot.is_dir():
        print(
            f"[wasi-demo] 错误: wasi-sdk 不完整 ({clang} / {sysroot}); "
            f"设 WASI_SDK_PATH 指向 {WASI_SDK_VERSION} 根目录",
            file=sys.stderr,
        )
        return 1
    if not SOURCE.is_file():
        print(f"[wasi-demo] 错误: 源缺失 {SOURCE}", file=sys.stderr)
        return 1

    args.out.parent.mkdir(parents=True, exist_ok=True)
    cmd = [
        str(clang),
        "--target=wasm32-wasip1",
        f"--sysroot={sysroot}",
        "-O2",
        str(SOURCE),
        "-o",
        str(args.out),
    ]
    print(f"[wasi-demo] 编译: {' '.join(cmd)}")
    result = subprocess.run(cmd, check=False)
    if result.returncode != 0:
        print(f"[wasi-demo] 错误: clang 退出码 {result.returncode}", file=sys.stderr)
        return 1
    size = args.out.stat().st_size
    print(f"[wasi-demo] 写出 {args.out}: {size} 字节 ({WASI_SDK_VERSION})")
    return 0


if __name__ == "__main__":
    sys.exit(main())