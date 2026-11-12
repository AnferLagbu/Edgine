#!/usr/bin/env python3
"""生成 W-7 端到端探针载荷: 最小合法 wasm 模块 (确定性字节).

产物 `hello.wasm` 被 `scripts/make_initramfs_cpio.py` 打包为 `bin/hello.wasm`,
init 的 `[WASM]` 探针 fork + exec 该路径; 内核 `exec.rs` 按魔数分派到用户态桩,
桩经 `EG_WASM_LOAD/RUN` 让解释器执行, 模块 `_start` 正常返回 0 ⇒ 退出码 0.

模块内容 (39 字节, 手写编码, 不依赖任何 wasm 工具链):

    magic/version  00 61 73 6D 01 00 00 00
    type           01 05 01 60 00 01 7F          () -> i32
    function       03 02 01 00                    1 个函数, 类型 0
    export         07 0A 01 06 5F 73 74 61 72 74 00 00   "_start"
    code           0A 06 01 04 00 41 00 0B         i32.const 0; end

用法: python3 scripts/make_probe_wasm.py --out other/build/<arch>/user/hello.wasm
退出码: 0 = 写出成功 (或自测通过); 1 = 参数/IO 失败.
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

# 字节逐项列在 docstring 中; 顺序即 wasm 段顺序, 改动须同步 docstring.
MODULE_BYTES = bytes(
    [
        0x00, 0x61, 0x73, 0x6D, 0x01, 0x00, 0x00, 0x00,
        0x01, 0x05, 0x01, 0x60, 0x00, 0x01, 0x7F,
        0x03, 0x02, 0x01, 0x00,
        0x07, 0x0A, 0x01, 0x06, 0x5F, 0x73, 0x74, 0x61, 0x72, 0x74, 0x00, 0x00,
        0x0A, 0x06, 0x01, 0x04, 0x00, 0x41, 0x00, 0x0B,
    ]
)


def self_test() -> int:
    """自证: 魔数 / 版本 / 导出名 / 长度."""
    assert MODULE_BYTES[:4] == b"\0asm", "魔数不符"
    assert MODULE_BYTES[4:8] == b"\x01\x00\x00\x00", "版本应为 1"
    assert b"_start" in MODULE_BYTES, "缺少 _start 导出名"
    assert len(MODULE_BYTES) == 39, f"长度应为 39, 实测 {len(MODULE_BYTES)}"
    print(f"自测通过 ({len(MODULE_BYTES)} 字节)")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", type=Path, help="输出路径")
    parser.add_argument("--self-test", action="store_true", help="只跑自测")
    args = parser.parse_args()

    if args.self_test:
        return self_test()
    if args.out is None:
        parser.error("--out 必需 (除非 --self-test)")

    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_bytes(MODULE_BYTES)
    print(f"[probe-wasm] 写出 {args.out}: {len(MODULE_BYTES)} 字节")
    return 0


if __name__ == "__main__":
    sys.exit(main())
