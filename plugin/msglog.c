// MessageLogger for Phoenix (native, arch-independent).
//
// Phoenix's core gateway registry is an array of {name ptr, name len, handler}; handlers are
//   Result handler(Store *store, const Event *ev, Ctx *ctx)        (Result: 32 bytes, Ok = w[0] == -1)
// with ev->name / ev->data = (ptr, len), data = raw dispatch JSON. Declaring the 32-byte struct
// return in C gives the same hidden result pointer Rust uses: rdi on x86_64, x8 on arm64. msglog_install() swaps the
// handler pointers of MESSAGE_CREATE / MESSAGE_UPDATE / MESSAGE_DELETE(_BULK) for wrappers:
//
// - create/update: cache each message's current content (raw, JSON-escaped) + edit history.
//   Edits are rewritten to   <new>\n-# <old line> (edited)\n-# ...   (newest history first).
// - delete: never removed. Re-rendered through the original MESSAGE_UPDATE handler as
//   ```ansi <ESC>[31m<old><ESC>[0m```  + history + "-# (deleted)"  (red block, small label).
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

typedef struct { const char *p; size_t n; } str_t;
typedef struct { str_t name, data; uint64_t rest[4]; } event_t;
typedef struct { uint64_t w[4]; } result_t;
typedef result_t (*handler_fn)(void *store, const event_t *ev, void *ctx);

static handler_fn o_create, o_update, o_delete;
static void (*g_log)(const char *msg);
#define ML_LOG(...) do { if (g_log) { char _b[256]; snprintf(_b, sizeof _b, __VA_ARGS__); g_log(_b); } } while (0)
static __thread int t_synthetic; // our own MESSAGE_UPDATE calls must pass through untouched

// --- growable buffer -------------------------------------------------------------------
typedef struct { char *p; size_t n, cap; } buf_t;
static void put(buf_t *b, const char *s, size_t n) {
    if (b->n + n + 1 > b->cap) { size_t c = (b->cap + n + 64) * 2; char *q = realloc(b->p, c); if (!q) return; b->p = q; b->cap = c; }
    memcpy(b->p + b->n, s, n); b->n += n; b->p[b->n] = 0;
}
static void puts_(buf_t *b, const char *s) { put(b, s, strlen(s)); }

// --- top-level JSON field (depth 1). Strings: raw contents without quotes. ---------------------
static int json_field(str_t j, const char *key, str_t *out) {
    size_t kl = strlen(key);
    int depth = 0;
    for (size_t i = 0; i < j.n; i++) {
        char c = j.p[i];
        if (c == '{' || c == '[') { depth++; continue; }
        if (c == '}' || c == ']') { depth--; continue; }
        if (c != '"') continue;
        size_t s = i + 1, e = s;
        while (e < j.n && j.p[e] != '"') { if (j.p[e] == '\\') e++; e++; }
        if (e >= j.n) return 0;
        size_t a = e + 1;
        while (a < j.n && j.p[a] == ' ') a++;
        if (!(a < j.n && j.p[a] == ':' && depth == 1 && e - s == kl && !memcmp(j.p + s, key, kl))) { i = e; continue; }
        size_t v = a + 1;
        while (v < j.n && j.p[v] == ' ') v++;
        if (v >= j.n) return 0;
        if (j.p[v] == '"') {
            size_t ve = v + 1;
            while (ve < j.n && j.p[ve] != '"') { if (j.p[ve] == '\\') ve++; ve++; }
            *out = (str_t){j.p + v + 1, ve - v - 1};
            return 1;
        }
        size_t ve = v; int d = 0;
        for (; ve < j.n; ve++) {
            char x = j.p[ve];
            if (x == '[' || x == '{') d++;
            else if (x == ']' || x == '}') { if (d == 0) break; if (--d == 0) { ve++; break; } }
            else if (x == ',' && d == 0) break;
        }
        *out = (str_t){j.p + v, ve - v};
        return 1;
    }
    return 0;
}

// Append raw JSON-escaped text as "-# " lines (every escaped \n starts a new subtext line).
static void put_subtext(buf_t *b, str_t raw, const char *suffix) {
    puts_(b, "-# ");
    for (size_t i = 0; i < raw.n; i++) {
        if (raw.p[i] == '\\' && i + 1 < raw.n) {
            if (raw.p[i + 1] == 'n') { puts_(b, "\\n-# "); i++; continue; }
            put(b, raw.p + i, 2); i++; continue;
        }
        put(b, raw.p + i, 1);
    }
    puts_(b, suffix);
}

// --- cache ------------------------------------------------------------------------------
#define CACHE_SLOTS 8192
typedef struct { uint64_t id; char channel[24], guild[24]; char *cur, *hist; int deleted; } entry_t;
static entry_t g_cache[CACHE_SLOTS];
static pthread_mutex_t g_lock = PTHREAD_MUTEX_INITIALIZER;

static uint64_t parse_id(str_t s) {
    uint64_t v = 0;
    if (!s.n) return 0;
    for (size_t i = 0; i < s.n && i < 20; i++) { if (s.p[i] < '0' || s.p[i] > '9') return 0; v = v * 10 + (uint64_t)(s.p[i] - '0'); }
    return v;
}
static char *dup_str(str_t s) { char *c = malloc(s.n + 1); if (c) { memcpy(c, s.p, s.n); c[s.n] = 0; } return c; }
static void copy_small(char *dst, str_t s) { size_t m = s.n < 23 ? s.n : 23; memcpy(dst, s.p, m); dst[m] = 0; }
// ponytail: direct-mapped cache, newest message wins a slot; a real map if collisions show up
static entry_t *slot(uint64_t id) { return &g_cache[(id ^ (id >> 22)) % CACHE_SLOTS]; }

static void remember(uint64_t mid, str_t content, str_t ch, str_t gd) {
    entry_t *e = slot(mid);
    if (e->id != mid) { free(e->cur); free(e->hist); memset(e, 0, sizeof *e); e->id = mid; }
    free(e->cur); e->cur = dup_str(content);
    copy_small(e->channel, ch); if (gd.n) copy_small(e->guild, gd);
}

// --- wrappers ---------------------------------------------------------------------------
static result_t w_create(void *store, const event_t *ev, void *ctx) {
    str_t id, content, ch, gd = {"", 0};
    if (json_field(ev->data, "id", &id) && json_field(ev->data, "content", &content) && json_field(ev->data, "channel_id", &ch)) {
        json_field(ev->data, "guild_id", &gd);
        uint64_t mid = parse_id(id);
        if (mid) { pthread_mutex_lock(&g_lock); remember(mid, content, ch, gd); pthread_mutex_unlock(&g_lock); }
    }
    return o_create(store, ev, ctx);
}

static result_t w_update(void *store, const event_t *ev, void *ctx) {
    str_t id, content, ch, gd = {"", 0};
    if (t_synthetic || !json_field(ev->data, "id", &id) || !json_field(ev->data, "content", &content) ||
        !json_field(ev->data, "channel_id", &ch))
        return o_update(store, ev, ctx); // e.g. embed-only updates
    json_field(ev->data, "guild_id", &gd);
    uint64_t mid = parse_id(id);
    buf_t rendered = {0};
    pthread_mutex_lock(&g_lock);
    entry_t *e = slot(mid);
    if (mid && e->id == mid && e->cur && (strlen(e->cur) != content.n || memcmp(e->cur, content.p, content.n))) {
        buf_t h = {0};                                   // new history = old content + previous history
        put_subtext(&h, (str_t){e->cur, strlen(e->cur)}, " (edited)");
        if (e->hist) { puts_(&h, "\\n"); puts_(&h, e->hist); }
        free(e->hist); e->hist = h.p;
    }
    if (mid) remember(mid, content, ch, gd);
    if (mid && e->hist) {                               // splice <new>\n<history> into the payload
        str_t d = ev->data;
        put(&rendered, d.p, (size_t)(content.p - d.p));
        put(&rendered, content.p, content.n);
        if (content.n) puts_(&rendered, "\\n");
        puts_(&rendered, e->hist);
        put(&rendered, content.p + content.n, d.n - (size_t)(content.p + content.n - d.p));
    }
    pthread_mutex_unlock(&g_lock);
    if (!rendered.p) return o_update(store, ev, ctx);
    event_t up = *ev; up.data = (str_t){rendered.p, rendered.n};
    result_t r = o_update(store, &up, ctx);
    free(rendered.p);
    return r;
}

static void mark_deleted(uint64_t mid, void *store, const event_t *orig, void *ctx) {
    buf_t j = {0};
    pthread_mutex_lock(&g_lock);
    entry_t *e = slot(mid);
    if (!mid || e->id != mid || !e->cur || e->deleted) { pthread_mutex_unlock(&g_lock); return; }
    e->deleted = 1;
    char head[160];
    snprintf(head, sizeof head, "{\"id\":\"%llu\",\"channel_id\":\"%s\"%s%s%s,\"content\":\"", (unsigned long long)mid,
             e->channel, e->guild[0] ? ",\"guild_id\":\"" : "", e->guild, e->guild[0] ? "\"" : "");
    puts_(&j, head);
    if (e->cur[0]) {                                   // red via diff highlighting: every line "- ..."
        puts_(&j, "```diff\\n- ");
        for (const char *c = e->cur; *c; c++) {
            if (c[0] == '\\' && c[1] == 'n') { puts_(&j, "\\n- "); c++; continue; }
            if (c[0] == '\\' && c[1]) { put(&j, c, 2); c++; continue; }
            put(&j, c, 1);
        }
        puts_(&j, "\\n```\\n");
    }
    if (e->hist) { puts_(&j, e->hist); puts_(&j, "\\n"); }
    puts_(&j, "-# (deleted)\"}");
    pthread_mutex_unlock(&g_lock);
    if (!j.p) return;
    event_t up = *orig;
    up.name = (str_t){"MESSAGE_UPDATE", 14};
    up.data = (str_t){j.p, j.n};
    t_synthetic = 1;
    result_t res = o_update(store, &up, ctx);
    t_synthetic = 0;
    ML_LOG("msglog: %llu marked deleted (%llx)", (unsigned long long)mid, (unsigned long long)res.w[0]);
    free(j.p);
}

static result_t w_delete(void *store, const event_t *ev, void *ctx) {
    str_t id, ids;
    if (json_field(ev->data, "id", &id)) mark_deleted(parse_id(id), store, ev, ctx);
    else if (json_field(ev->data, "ids", &ids))
        for (size_t i = 0; i < ids.n; i++) {
            if (ids.p[i] != '"') continue;
            size_t e = i + 1; while (e < ids.n && ids.p[e] != '"') e++;
            mark_deleted(parse_id((str_t){ids.p + i + 1, e - i - 1}), store, ev, ctx);
            i = e;
        }
    result_t ok = {{UINT64_MAX, 0, 0, 0}}; // Ok(()): the message is never removed
    return ok;
}

// slots: addresses of the handler fields of the registry entries (already relocated pointers).
// Caller makes them writable.
void msglog_install(handler_fn *create_slot, handler_fn *update_slot, handler_fn *delete_slot, handler_fn *bulk_slot,
                    void (*log)(const char *)) {
    g_log = log;
    o_create = *create_slot; o_update = *update_slot; o_delete = *delete_slot;
    *create_slot = w_create; *update_slot = w_update; *delete_slot = w_delete;
    if (bulk_slot) *bulk_slot = w_delete;
    ML_LOG("msglog: installed (create %p update %p delete %p)", (void *)o_create, (void *)o_update, (void *)o_delete);
}
