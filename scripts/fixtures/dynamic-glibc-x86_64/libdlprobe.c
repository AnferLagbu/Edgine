/* D7 L1: 第二个共享对象，供 glibc dlopen/dlsym/dlclose 探针装载。
 * 不依赖 libc，保持 NEEDED 为空；全局变量与函数都进 .dynsym，用于验证
 * 文件映射、GLOB_DAT 重定位、符号查找与卸载后的重新初始化。 */
int probe_counter = 5;

int probe_add(int value) {
    probe_counter += value;
    return probe_counter;
}

int probe_get(void) {
    return probe_counter;
}
