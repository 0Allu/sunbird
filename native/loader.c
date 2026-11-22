#define _GNU_SOURCE
#include <jni.h>
#include <android/log.h>
#include <dlfcn.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

#define TAG "GateProbe"
#define LOGI(...) __android_log_print(ANDROID_LOG_ERROR, TAG, __VA_ARGS__)
#define SRC "/data/local/tmp/gatepayload.so"
#define DST "/data/user/0/com.discord/files/gatepayload.so"

static int copy_file(const char *src, const char *dst) {
    int in = open(src, O_RDONLY);
    if (in < 0) return -1;
    int out = open(dst, O_WRONLY | O_CREAT | O_TRUNC, 0700);
    if (out < 0) { close(in); return -1; }
    char buf[65536]; ssize_t n;
    while ((n = read(in, buf, sizeof(buf))) > 0) {
        ssize_t off = 0;
        while (off < n) {
            ssize_t w = write(out, buf + off, (size_t)(n - off));
            if (w <= 0) { close(in); close(out); return -1; }
            off += w;
        }
    }
    close(in); close(out);
    return n < 0 ? -1 : 0;
}

static void *worker(void *unused) {
    (void)unused;
    for (int i = 0; i < 600; i++) {
        if (access(SRC, R_OK) == 0) {
            if (copy_file(SRC, DST) == 0) LOGI("payload copied");
            else LOGI("payload copy failed");
            void *h = dlopen(DST, RTLD_NOW);
            if (!h) {
                LOGI("dlopen(%s) failed: %s", DST, dlerror());
                h = dlopen(SRC, RTLD_NOW);
                if (!h) { LOGI("dlopen(%s) failed: %s", SRC, dlerror()); return NULL; }
            }
            void (*init)(void) = (void (*)(void))dlsym(h, "gate_payload_init");
            if (init) { LOGI("payload init %p", (void*)init); init(); }
            else LOGI("payload init missing");
            return NULL;
        }
        usleep(100000);
    }
    LOGI("payload %s not found", SRC);
    return NULL;
}

JNIEXPORT jint JNI_OnLoad(JavaVM *vm, void *reserved) {
    (void)vm; (void)reserved;
    pthread_t th;
    if (pthread_create(&th, NULL, worker, NULL) == 0) pthread_detach(th);
    return JNI_VERSION_1_6;
}
