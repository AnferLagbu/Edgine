// D7 L1: 上游 glibc loader 的第二共享对象装载探针.
//
// 覆盖:
//   1. dlopen("/lib64/libdlprobe.so", RTLD_NOW|RTLD_LOCAL) 返回句柄;
//   2. dlsym 解析 probe_add/probe_get, 首次调用得到 12;
//   3. dlclose 真正卸载后重新 dlopen, 全局 probe_counter 重新初始化为 5;
//   4. dlsym 失败路径返回 NULL, 不误报成功.
//
// 全部成功打印 "[DLOPEN] PASS"，任一失败打印 "[DLOPEN] FAIL: ..." 并非零退出.

#define _GNU_SOURCE
#include <dlfcn.h>
#include <errno.h>
#include <stdio.h>

typedef int (*add_fn)(int);
typedef int (*get_fn)(void);

static void *open_probe(void) {
    errno = 0;
    void *h = dlopen("/lib64/libdlprobe.so", RTLD_NOW | RTLD_LOCAL);
    if (h == NULL) {
        printf("[DLOPEN] FAIL: dlopen errno=%d dlerror=%s\n", errno, dlerror());
        return NULL;
    }
    return h;
}

int main(void) {
    setvbuf(stdout, NULL, _IONBF, 0);
    printf("[DLOPEN] begin\n");

    void *h = open_probe();
    if (h == NULL) return 1;

    add_fn add = (add_fn)dlsym(h, "probe_add");
    get_fn get = (get_fn)dlsym(h, "probe_get");
    if (add == NULL || get == NULL) {
        printf("[DLOPEN] FAIL: dlsym add=%p get=%p dlerror=%s\n",
               (void *)add, (void *)get, dlerror());
        return 1;
    }

    int first = add(7);
    int value = get();
    printf("[DLOPEN] first=%d value=%d\n", first, value);
    if (first != 12 || value != 12) {
        printf("[DLOPEN] FAIL: unexpected first/value\n");
        return 1;
    }

    void *missing = dlsym(h, "probe_missing_symbol");
    if (missing != NULL) {
        printf("[DLOPEN] FAIL: missing symbol resolved\n");
        return 1;
    }

    errno = 0;
    int close_rc = dlclose(h);
    if (close_rc != 0) {
        printf("[DLOPEN] FAIL: dlclose rc=%d errno=%d dlerror=%s\n",
               close_rc, errno, dlerror());
        return 1;
    }

    h = open_probe();
    if (h == NULL) return 1;
    add = (add_fn)dlsym(h, "probe_add");
    get = (get_fn)dlsym(h, "probe_get");
    if (add == NULL || get == NULL) {
        printf("[DLOPEN] FAIL: reload dlsym failed\n");
        return 1;
    }
    value = get();
    if (value != 5) {
        printf("[DLOPEN] FAIL: unload did not reset counter value=%d\n", value);
        return 1;
    }
    if (dlclose(h) != 0) {
        printf("[DLOPEN] FAIL: second dlclose\n");
        return 1;
    }

    printf("[DLOPEN] PASS\n");
    return 0;
}
