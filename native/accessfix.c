// Phoenix patches for liblibdiscord_wasm_host.so (Discord 356.7 / 356207, x86_64).
//
// access: 0x2eac1c4 = libdiscord_ops_surfaces::client_access evaluator. Reads experiments
//         "2026-11-phoenix-app-access" / "2026-11-phoenix-rollout" (+ overrides), returns
//         Option<bool> (0/1, 2=unknown). Force Some(true) -> ClientAccessGate never builds.
// staff:  0x1f76640 = is_staff(state, force). True if force / LIBDISCORD_DEV_ANALYTICS /
//         debug flag, else current_user.flags & 1 (STAFF). Force true -> dev tools,
//         Experiment Overrides etc. 17 call sites, all via GOT slot 0x6bc5f30.
#define _GNU_SOURCE
#include <android/log.h>
#include <link.h>
#include <pthread.h>
#include <stdint.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#define TAG "GateProbe"
#define LOGI(...) __android_log_print(ANDROID_LOG_ERROR, TAG, __VA_ARGS__)

static const struct {
    const char *name;
    uint32_t off;
    uint8_t expect[5];
} patches[] = {
    {"access", 0x2eac1c4u, {0x55, 0x41, 0x57, 0x41, 0x56}}, // push rbp; push r15; push r14
    {"staff",  0x1f76640u, {0x53, 0x48, 0x83, 0xec, 0x20}}, // push rbx; sub rsp, 0x20
};
static const uint8_t ret_true[] = {0xb0, 0x01, 0xc3}; // mov al, 1; ret

static int find_cb(struct dl_phdr_info *info, size_t size, void *data) {
    (void)size;
    if (info->dlpi_name && strstr(info->dlpi_name, "liblibdiscord_wasm_host.so")) {
        *(uintptr_t *)data = info->dlpi_addr;
        return 1;
    }
    return 0;
}

static void patch(uint8_t *base) {
    size_t ps = (size_t)sysconf(_SC_PAGESIZE);
    for (size_t i = 0; i < sizeof patches / sizeof *patches; i++) {
        uint8_t *t = base + patches[i].off;
        if (memcmp(t, patches[i].expect, sizeof patches[i].expect) != 0) {
            LOGI("patch %s: unexpected bytes at %p, wrong build? skipped", patches[i].name, (void *)t);
            continue;
        }
        uintptr_t page = (uintptr_t)t & ~(ps - 1);
        if (mprotect((void *)page, ps * 2, PROT_READ | PROT_WRITE | PROT_EXEC) != 0) {
            LOGI("patch %s: mprotect failed", patches[i].name);
            continue;
        }
        memcpy(t, ret_true, sizeof ret_true);
        __builtin___clear_cache((char *)t, (char *)t + sizeof ret_true);
        mprotect((void *)page, ps * 2, PROT_READ | PROT_EXEC);
        LOGI("patch %s: applied at %p", patches[i].name, (void *)t);
    }
}

static void *worker(void *unused) {
    (void)unused;
    // ponytail: 1ms poll for the lib load; hook dlopen if this ever loses the race.
    for (int i = 0; i < 120000; i++) {
        uintptr_t base = 0;
        dl_iterate_phdr(find_cb, &base);
        if (base) { patch((uint8_t *)base); return NULL; }
        usleep(1000);
    }
    LOGI("liblibdiscord_wasm_host.so never loaded");
    return NULL;
}

__attribute__((visibility("default"))) void gate_payload_init(void) {
    pthread_t th;
    if (pthread_create(&th, NULL, worker, NULL) == 0) pthread_detach(th);
}
