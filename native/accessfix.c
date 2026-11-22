// Phoenix patches for liblibdiscord_wasm_host.so (Discord 356.7 / 356207, x86_64).
//
// access: 0x2eac1c4 = libdiscord_ops_surfaces::client_access evaluator. Reads experiments
//         "2026-11-phoenix-app-access" / "2026-11-phoenix-rollout" (+ overrides), returns
//         Option<bool> (0/1, 2=unknown). Patched to return Some(true), so the
//         ClientAccessGate is never built.
//
// staff:  Staff is checked as (user.flags | flag_override.unwrap_or(0)) & 1, where
//         user.flags is at state+0x84f0 and flag_override is Option<u32> with its tag at
//         state+0x8580 and its value at +0x8588. The check is inlined in about 30 places
//         and has 6 standalone copies (one of them gates the "Show Dev Tools Widget"
//         setter), so patching each site doesn't scale. Instead we hook one hot copy,
//         is_staff(handle, force) at 0x1f76640 (17 callers). The hook takes the same store
//         read-guard the original takes, sets flag_override = Some(STAFF) in the state,
//         and returns true. Every copy reads that field, so all of them pass. The write
//         happens again on every call, so the flag comes back if the state is rebuilt.
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

#define ACCESS_OFF     0x2eac1c4u
#define STAFF_OFF      0x1f76640u
#define GOT_READ_STATE 0x6ba1e68u  // GOT slot: read_state(&guard, handle, key)
#define STATE_KEY      0x67f9420u  // key the original is_staff passes
#define GUARD_DROP     0x1b8d403u  // drop(&guard)
#define ST_OVERRIDE_TAG 0x8580u
#define ST_OVERRIDE_VAL 0x8588u
#define USER_FLAG_STAFF 1u

typedef void (*read_state_fn)(void *guard, void *handle, const void *key);
typedef void (*drop_fn)(void *guard);

static uint8_t *g_base;
static volatile int g_logged;

static uint8_t is_staff_hook(void *handle, uint32_t force) {
    (void)force;
    uint64_t guard[8] = {0};
    (*(read_state_fn *)(g_base + GOT_READ_STATE))(guard, handle, g_base + STATE_KEY);
    uint8_t *state = (uint8_t *)guard[0];
    if (state) {
        // ponytail: plain write under a read guard. It's a 1-byte tag plus an OR into a u32
        // that we write again on every call. Hook the reducer instead if a torn read ever shows.
        state[ST_OVERRIDE_TAG] = 1;
        *(volatile uint32_t *)(state + ST_OVERRIDE_VAL) |= USER_FLAG_STAFF;
        if (!g_logged) { g_logged = 1; LOGI("staff: flag_override set on state %p", (void *)state); }
    }
    ((drop_fn)(g_base + GUARD_DROP))(guard);
    return 1;
}

static int find_cb(struct dl_phdr_info *info, size_t size, void *data) {
    (void)size;
    if (info->dlpi_name && strstr(info->dlpi_name, "liblibdiscord_wasm_host.so")) {
        *(uintptr_t *)data = info->dlpi_addr;
        return 1;
    }
    return 0;
}

static void write_code(const char *name, uint8_t *t, const uint8_t *expect, size_t elen,
                       const uint8_t *code, size_t clen) {
    if (memcmp(t, expect, elen) != 0) {
        LOGI("patch %s: unexpected bytes at %p, wrong build? skipped", name, (void *)t);
        return;
    }
    size_t ps = (size_t)sysconf(_SC_PAGESIZE);
    uintptr_t page = (uintptr_t)t & ~(ps - 1);
    if (mprotect((void *)page, ps * 2, PROT_READ | PROT_WRITE | PROT_EXEC) != 0) {
        LOGI("patch %s: mprotect failed", name);
        return;
    }
    memcpy(t, code, clen);
    __builtin___clear_cache((char *)t, (char *)t + clen);
    mprotect((void *)page, ps * 2, PROT_READ | PROT_EXEC);
    LOGI("patch %s: applied at %p", name, (void *)t);
}

static void patch(uint8_t *base) {
    g_base = base;

    static const uint8_t access_expect[] = {0x55, 0x41, 0x57, 0x41, 0x56}; // push rbp; push r15; push r14
    static const uint8_t ret_true[] = {0xb0, 0x01, 0xc3};                  // mov al, 1; ret
    write_code("access", base + ACCESS_OFF, access_expect, sizeof access_expect, ret_true, sizeof ret_true);

    // push rbx; sub rsp,0x20; mov bl,1; test esi,esi
    static const uint8_t staff_expect[] = {0x53, 0x48, 0x83, 0xec, 0x20, 0xb3, 0x01, 0x85, 0xf6};
    uint8_t jmp[12] = {0x48, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0xff, 0xe0}; // movabs rax, hook; jmp rax
    uint64_t hook = (uint64_t)(uintptr_t)is_staff_hook;
    memcpy(jmp + 2, &hook, 8);
    write_code("staff", base + STAFF_OFF, staff_expect, sizeof staff_expect, jmp, sizeof jmp);
}

static void *worker(void *unused) {
    (void)unused;
    // ponytail: poll every 1 ms until the lib loads. Hook dlopen instead if this ever loses the race.
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
