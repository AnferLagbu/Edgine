#define _GNU_SOURCE
#include <errno.h>
#include <pthread.h>
#include <stdio.h>
#include <sys/syscall.h>
#include <unistd.h>

static __thread int tls_var = 7;
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
    printf("[PTHREAD] PASS\n");
    return 0;
}
