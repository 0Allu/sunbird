// libplg.so: native plugin host for Phoenix.
//
// Loaded as a dependency of liblibdiscord_wasm_host.so: the patcher renames that library's
// DT_NEEDED "liblog.so" to "libplg.so" (same length), and libplg.so itself needs liblog.so, so
// symbol lookup is unchanged. Our constructor runs after the host lib is mapped and relocated but
// before its own constructors and before the app starts, so registry edits are seen from the start.
//
// The patcher writes `g_cfg` (found by its magic) with the enabled plugins and the offsets it
// located in this exact Discord build, so nothing here is build-specific.
#include <android/log.h>
#include <link.h>
#include <stdint.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#include "msglog.c"

#define TAG "PhoenixPlugins"
#define LOGI(...) __android_log_print(ANDROID_LOG_INFO, TAG, __VA_ARGS__)

enum { PLUGIN_MESSAGE_LOGGER = 1u << 0 };

struct config {
    char magic[16];                 // "PHXPLUG-CONFIG1" until the patcher fills it in
    uint32_t version, flags;        // flags: PLUGIN_*
    uint64_t msg_create_slot;       // offsets in the host lib of the registry handler fields
    uint64_t msg_update_slot;
    uint64_t msg_delete_slot;
    uint64_t msg_delete_bulk_slot;
};
__attribute__((used, visibility("default"))) volatile struct config g_cfg = {"PHXPLUG-CONFIG1", 1, 0, 0, 0, 0, 0};

static int find_host(struct dl_phdr_info *info, size_t size, void *data) {
    (void)size;
    if (info->dlpi_name && strstr(info->dlpi_name, "liblibdiscord_wasm_host.so")) {
        *(uintptr_t *)data = info->dlpi_addr;
        return 1;
    }
    return 0;
}

static void log_line(const char *m) { LOGI("%s", m); }

static int writable(uintptr_t a, uintptr_t b, int prot) {
    size_t ps = (size_t)sysconf(_SC_PAGESIZE);
    uintptr_t lo = a & ~(ps - 1), hi = (b + ps - 1) & ~(ps - 1);
    return mprotect((void *)lo, hi - lo, prot);
}

__attribute__((constructor)) static void plugins_init(void) {
    if (!g_cfg.flags) return;
    uintptr_t base = 0;
    dl_iterate_phdr(find_host, &base);
    if (!base) { LOGI("host lib not found"); return; }

    if (g_cfg.flags & PLUGIN_MESSAGE_LOGGER) {
        uintptr_t s[4] = {base + g_cfg.msg_create_slot, base + g_cfg.msg_update_slot,
                          base + g_cfg.msg_delete_slot, base + g_cfg.msg_delete_bulk_slot};
        uintptr_t lo = s[0], hi = s[0];
        for (int i = 1; i < 4; i++) { if (s[i] < lo) lo = s[i]; if (s[i] > hi) hi = s[i]; }
        if (writable(lo, hi + 8, PROT_READ | PROT_WRITE)) { LOGI("MessageLogger: mprotect failed"); return; }
        msglog_install((handler_fn *)s[0], (handler_fn *)s[1], (handler_fn *)s[2], (handler_fn *)s[3], log_line);
        writable(lo, hi + 8, PROT_READ);
    }
}
