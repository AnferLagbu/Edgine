#!/usr/bin/env python3
"""再生成 WASM P3 WASI 覆盖探针 `scripts/fixtures/wasi_p3_probe.wasm`.

载荷源 `scripts/fixtures/wasi_p3_probe.c` 是 wasi-sdk 编译的真实工具链模块, 由
`scripts/make_initramfs_cpio.py` 打包为 `bin/wasi_p3.wasm`; init 的 `[WASMI-P3]`
探针 fork + exec 它, 由用户态 wasmi 运行时 `wasmrun` 解释执行 (P4 后唯一路径),
host 层把 wasi_snapshot_preview1 调用翻译到 Edgine syscall.

载荷覆盖 P2 已经在真机跑过的 stdio/文件/链接/目录之外的剩余入口:
`sched_yield`、`fd_allocate`、`fd_advise`、`fd_renumber`、`fd_dup`、
`path_filestat_set_times`、`fd_prestat_*`、`fd_seek(SEEK_END)`、多条目
`fd_readdir`, 以及 ENOENT/EEXIST/ENOTEMPTY/ENOTDIR/EBADF 错误分支.

为什么 committed fixture 而非构建期编译 (与 make_wasi_demo.py 同口径):
1. 构建期不依赖 wasi-sdk —— CI / 干净环境只需复制预编译产物;
2. provenance 明确 —— de-risk 价值在「wasi-sdk 真产物能在用户态 wasmi 下跑通」,
   源与钉住的 SDK 版本一并入库, 升级时用本脚本重生成并同批提交.

用法:
    WASI_SDK_PATH=~/wasi-sdk/wasi-sdk-34.0-x86_64-linux \
        python3 scripts/make_wasi_p3_probe.py --out scripts/fixtures/wasi_p3_probe.wasm

退出码: 0 = 写出成功; 1 = 参数/工具链/IO 失败.
"""

from __future__ import annotations

import argparse
import os
import subprocess
import sys
from pathlib import Path

PROJECT_ROOT = Path(__file__).resolve().parent.parent
SOURCE = PROJECT_ROOT / "scripts" / "fixtures" / "wasi_p3_probe.c"
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
            f"[wasi-p3] 错误: wasi-sdk 不完整 ({clang} / {sysroot}); "
            f"设 WASI_SDK_PATH 指向 {WASI_SDK_VERSION} 根目录",
            file=sys.stderr,
        )
        return 1
    if not SOURCE.is_file():
        print(f"[wasi-p3] 错误: 源缺失 {SOURCE}", file=sys.stderr)
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
    print(f"[wasi-p3] 编译: {' '.join(cmd)}")
    result = subprocess.run(cmd, check=False)
    if result.returncode != 0:
        print(f"[wasi-p3] 错误: clang 退出码 {result.returncode}", file=sys.stderr)
        return 1
    size = args.out.stat().st_size
    print(f"[wasi-p3] 写出 {args.out}: {size} 字节 ({WASI_SDK_VERSION})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
