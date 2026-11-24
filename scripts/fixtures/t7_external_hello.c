/* T7-c 外部 Linux 程序验收载荷: 最小静态 musl hello.
 *
 * 编译 (Zig 0.13, 不参与仓内构建):
 *   zig cc -target x86_64-linux-musl -static -O2 -s -o hello hello.c
 *   zig cc -target aarch64-linux-musl -static -O2 -s -o hello_arm hello.c
 *
 * 只使用 puts + 正常返回: 覆盖 execve 装载、用户态 write、exit 三类 Linux
 * ABI 面; 不引入动态链接器/vDSO/glibc.
 */
#include <stdio.h>

int main(void) {
    puts("hello from external musl");
    return 0;
}
