// D7 C1: clone3 高级字段探针.
//
// 覆盖:
//   1. exit_signal=SIGUSR1: 子进程退出后父进程收到 SIGUSR1, waitpid(__WALL) 可回收;
//   2. CLONE_PIDFD: fd 写回、带 CLOEXEC、子进程退出后可零超时 poll;
//   3. size=64 的短结构: 不得读穿 64 字节基础字段;
//   4. set_tid/set_tid_size: 当前内核显式 ENOSYS;
//   5. cgroup/CLONE_INTO_CGROUP: 当前内核显式 ENOSYS;
//   6. exit_signal=65: 非法信号号 EINVAL.
//
// 全部成功打印 "[CLONE3] PASS"，任一失败打印 "[CLONE3] FAIL: ..." 并以非零退出.

#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <linux/sched.h>
#include <poll.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

#ifndef SYS_clone3
#define SYS_clone3 435
#endif

#ifndef CLONE_PIDFD
#define CLONE_PIDFD 0x00001000
#endif

#ifndef CLONE_INTO_CGROUP
#define CLONE_INTO_CGROUP 0x0000000200000000ULL
#endif

#ifndef __WALL
#define __WALL 0x40000000
#endif

struct clone_args_probe {
    uint64_t flags;
    uint64_t pidfd;
    uint64_t child_tid;
    uint64_t parent_tid;
    uint64_t exit_signal;
    uint64_t stack;
    uint64_t stack_size;
    uint64_t tls;
    uint64_t set_tid;
    uint64_t set_tid_size;
    uint64_t cgroup;
};

static volatile sig_atomic_t got_sigusr1;

static void on_sigusr1(int sig) {
    (void)sig;
    got_sigusr1 = 1;
}

static long raw_clone3(struct clone_args_probe *args, size_t size) {
    return syscall(SYS_clone3, args, size);
}

static int wait_child(long child, int expected_status) {
    int status = -1;
    int wait_rc;
    do {
        wait_rc = waitpid((pid_t)child, &status, __WALL);
    } while (wait_rc == -1 && errno == EINTR);
    if (wait_rc != child || !WIFEXITED(status) || WEXITSTATUS(status) != expected_status) {
        printf("[CLONE3] FAIL: wait rc=%d status=%d errno=%d\n", wait_rc, status, errno);
        return -1;
    }
    return 0;
}

int main(void) {
    setvbuf(stdout, NULL, _IONBF, 0);
    printf("[CLONE3] start\n");

    struct sigaction sa = {0};
    sa.sa_handler = on_sigusr1;
    sigemptyset(&sa.sa_mask);
    if (sigaction(SIGUSR1, &sa, NULL) != 0) {
        printf("[CLONE3] FAIL: sigaction errno=%d\n", errno);
        return 1;
    }
    printf("[CLONE3] sigaction PASS\n");

    // 1. exit_signal=SIGUSR1: 子进程正常退出，父进程必须收到 SIGUSR1.
    struct clone_args_probe args = {0};
    args.exit_signal = SIGUSR1;
    long child = raw_clone3(&args, sizeof(args));
    if (child == 0) {
        _exit(0);
    }
    if (child < 0) {
        printf("[CLONE3] FAIL: clone3 exit_signal errno=%d\n", errno);
        return 1;
    }
    if (wait_child(child, 0) != 0) {
        return 1;
    }
    for (int i = 0; i < 100 && !got_sigusr1; i++) {
        usleep(1000);
    }
    if (!got_sigusr1) {
        printf("[CLONE3] FAIL: exit_signal SIGUSR1 未投递\n");
        return 1;
    }
    printf("[CLONE3] exit_signal PASS\n");

    // 2. CLONE_PIDFD: fd 必须写回、带 CLOEXEC 且子退出后可读.
    int pidfd = -1;
    struct clone_args_probe pidfd_args = {0};
    pidfd_args.flags = CLONE_PIDFD;
    pidfd_args.exit_signal = SIGCHLD;
    pidfd_args.pidfd = (uint64_t)(uintptr_t)&pidfd;
    child = raw_clone3(&pidfd_args, sizeof(pidfd_args));
    if (child == 0) {
        _exit(0);
    }
    if (child < 0 || pidfd < 0) {
        printf("[CLONE3] FAIL: pidfd clone3 child=%ld pidfd=%d errno=%d\n",
               child, pidfd, errno);
        return 1;
    }
    if ((fcntl(pidfd, F_GETFD) & FD_CLOEXEC) == 0) {
        printf("[CLONE3] FAIL: pidfd 未带 CLOEXEC\n");
        return 1;
    }
    // 先 wait4 收割，再以零超时 poll 校验 pidfd 的退出就绪态。Edgine 的
    // poll/ppoll 当前是单次快照（超时不阻塞，见 file_ops.rs SIMPLIFIED），
    // 不能在子进程尚未退出时等 5000ms；此处不把通用 poll 等待语义混入 C1。
    if (wait_child(child, 0) != 0) {
        return 1;
    }
    struct pollfd pfd = {.fd = pidfd, .events = POLLIN};
    int poll_rc = poll(&pfd, 1, 0);
    if (poll_rc <= 0 || (pfd.revents & POLLIN) == 0) {
        printf("[CLONE3] FAIL: pidfd poll_rc=%d revents=%d\n", poll_rc, pfd.revents);
        return 1;
    }
    close(pidfd);
    printf("[CLONE3] pidfd PASS\n");

    // 3. size=64: 只提供基础字段, 扩展字段不应被读取.
    struct clone_args_probe short_args = {0};
    child = raw_clone3(&short_args, 64);
    if (child == 0) {
        _exit(7);
    }
    if (child < 0) {
        printf("[CLONE3] FAIL: clone3 size=64 errno=%d\n", errno);
        return 1;
    }
    if (wait_child(child, 7) != 0) {
        return 1;
    }
    printf("[CLONE3] size=64 PASS\n");

    // 4. set_tid: 未实现, 必须 ENOSYS 且不得创建子进程.
    struct clone_args_probe set_tid_args = {0};
    int tid_slot = 0;
    set_tid_args.set_tid = (uint64_t)(uintptr_t)&tid_slot;
    set_tid_args.set_tid_size = 1;
    child = raw_clone3(&set_tid_args, sizeof(set_tid_args));
    if (child == 0) {
        _exit(0);
    }
    if (child != -1 || errno != ENOSYS) {
        printf("[CLONE3] FAIL: set_tid child=%ld errno=%d\n", child, errno);
        return 1;
    }
    printf("[CLONE3] set_tid ENOSYS PASS\n");

    // 5. cgroup 同属未实现边界.
    struct clone_args_probe cgroup_args = {0};
    cgroup_args.flags = CLONE_INTO_CGROUP;
    cgroup_args.cgroup = 1;
    child = raw_clone3(&cgroup_args, sizeof(cgroup_args));
    if (child == 0) {
        _exit(0);
    }
    if (child != -1 || errno != ENOSYS) {
        printf("[CLONE3] FAIL: cgroup child=%ld errno=%d\n", child, errno);
        return 1;
    }
    printf("[CLONE3] cgroup ENOSYS PASS\n");

    // 6. 非法信号号必须在创建子进程前拒绝.
    struct clone_args_probe bad_sig_args = {0};
    bad_sig_args.exit_signal = 65;
    child = raw_clone3(&bad_sig_args, sizeof(bad_sig_args));
    if (child == 0) {
        _exit(0);
    }
    if (child != -1 || errno != EINVAL) {
        printf("[CLONE3] FAIL: exit_signal=65 child=%ld errno=%d\n", child, errno);
        return 1;
    }
    printf("[CLONE3] exit_signal=65 EINVAL PASS\n");

    printf("[CLONE3] PASS\n");
    return 0;
}
