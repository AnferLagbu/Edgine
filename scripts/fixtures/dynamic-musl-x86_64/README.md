# 动态 musl 验收 fixture (x86_64)

用于 D5/D6：`hello_dyn` 带 `PT_INTERP=/lib/ld-musl-x86_64.so.1` 且
`NEEDED=libc.musl-x86_64.so.1`，由 init 的 `dyn_probe` 真 `execve`。

| 文件 | 来源 |
|---|---|
| `ld-musl-x86_64.so.1` | Alpine 3.20 minirootfs `/lib/ld-musl-x86_64.so.1`（musl MIT） |
| `libc.musl-x86_64.so.1` | 同一对象的副本（Alpine 中为符号链接；initramfs 不支持符号链接） |
| `hello_dyn` | `gcc -fPIE -pie -nostdlib -nostartfiles -Wl,-e,main -Wl,--dynamic-linker=/lib/ld-musl-x86_64.so.1 -L<alpine>/lib -l:libc.musl-x86_64.so.1 hello.c` |

`SHA256SUMS` 固定字节；构建侧只复制，不在构建期联网。
