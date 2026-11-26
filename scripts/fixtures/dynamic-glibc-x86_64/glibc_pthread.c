#define _GNU_SOURCE
#include <errno.h>
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <sys/rseq.h>
#include <sys/syscall.h>
#include <unistd.h>

#ifndef RSEQ_SIG
#define RSEQ_SIG 0x53053053u
#endif

static __thread int tls_var = 7;
static struct rseq worker_rseq __attribute__((aligned(32)));
static struct rseq_cs worker_rseq_cs __attribute__((aligned(32)));
static volatile int worker_rseq_registered;
static pthread_key_t key;
static pthread_mutex_t lock = PTHREAD_MUTEX_INITIALIZER;
static int shared;

static void *worker(void *arg) {
    (void)arg;
    printf("[PTHREAD] worker self=%lu tid=%ld tls=%d\n",
           (unsigned long)pthread_self(), (long)syscall(SYS_gettid), tls_var);
    tls_var = 42;
    int *v = pthread_getspecific(key);
    printf("[PTHREAD] worker key=%d\n", v ? *v : -1);
    pthread_mutex_lock(&lock);
    shared++;
    pthread_mutex_unlock(&lock);

    /* D7 R1 退出清理腿: 先把 glibc 自注册区注销, 再以线程私有堆区注册并
     * 设置 rseq_cs; 线程退出时内核必须清除该字段. 父线程 join 后检查. */
    struct rseq *glibc_area =
        (struct rseq *)((char *)__builtin_thread_pointer() + __rseq_offset);
    unsigned int len = __rseq_size < 32 ? 32 : __rseq_size;
    errno = 0;
    long rc = syscall(SYS_rseq, glibc_area, len, RSEQ_FLAG_UNREGISTER, RSEQ_SIG);
    printf("[PTHREAD] worker rseq unregister rc=%ld errno=%d\n", rc, errno);
    if (rc == 0) {
        errno = 0;
        rc = syscall(SYS_rseq, &worker_rseq, sizeof(worker_rseq), 0, RSEQ_SIG);
        printf("[PTHREAD] worker rseq register rc=%ld errno=%d area=%p cpu=%u/%u\n",
               rc, errno, (void *)&worker_rseq,
               worker_rseq.cpu_id_start, worker_rseq.cpu_id);
        if (rc == 0) {
            worker_rseq.rseq_cs = (uint64_t)(uintptr_t)&worker_rseq_cs;
            worker_rseq_registered = 1;
        }
    }
    return (void *)0x1234;
}

int main(void) {
    /* 无缓冲输出，避免 QEMU 串口下 glibc stdio 全缓冲掩盖失败点。 */
    setvbuf(stdout, NULL, _IONBF, 0);
    int main_value = 99;
    tls_var = 11;
    printf("[PTHREAD] main self=%lu tid=%ld tls=%d\n",
           (unsigned long)pthread_self(), (long)syscall(SYS_gettid), tls_var);

    errno = 0;
    int rc = pthread_key_create(&key, NULL);
    printf("[PTHREAD] key_create rc=%d errno=%d\n", rc, errno);
    if (rc != 0) return 1;
    rc = pthread_setspecific(key, &main_value);
    printf("[PTHREAD] setspecific rc=%d errno=%d\n", rc, errno);
    if (rc != 0) return 1;

    pthread_t th;
    /* pthread_create 的返回值才是错误码；glibc 内部探测可能残留 errno，
       不能把 errno 单独当失败判据。 */
    errno = 0;
    rc = pthread_create(&th, NULL, worker, NULL);
    printf("[PTHREAD] create rc=%d errno=%d\n", rc, errno);
    if (rc != 0) return 1;

    void *ret = NULL;
    errno = 0;
    rc = pthread_join(th, &ret);
    printf("[PTHREAD] join rc=%d errno=%d ret=%p shared=%d tls=%d\n",
           rc, errno, ret, shared, tls_var);
    if (rc != 0 || ret != (void *)0x1234 || shared != 1 || tls_var != 11) {
        printf("[PTHREAD] FAIL\n");
        return 1;
    }
    if (worker_rseq_registered) {
        if (worker_rseq.rseq_cs == 0) {
            printf("[PTHREAD] RSEQ EXIT PASS\n");
        } else {
            printf("[PTHREAD] RSEQ EXIT FAIL cs=%llu\n",
                   (unsigned long long)worker_rseq.rseq_cs);
        }
    } else {
        printf("[PTHREAD] RSEQ EXIT SKIP\n");
    }
    printf("[PTHREAD] PASS\n");
    return 0;
}
