// Cloudflare Worker: CORS pass-through for the Discord APK tracker, which redirects to Google's
// CDN (no CORS headers). Only /tracker/index and /tracker/download/<versionCode>/<split> are allowed.
// Splits of a given versionCode never change, so they are kept in the edge cache (Cache API, per
// data center) for a year and marked immutable for browsers; the index is cached for 5 minutes.
// Deploy: `npx wrangler deploy web/worker/proxy.js --name phoenix-apk-proxy --compatibility-date 2026-11-01`
const UPSTREAM = 'https://tracker.vendetta.rocks';
const CORS = {
  'Access-Control-Allow-Origin': '*',
  'Access-Control-Allow-Methods': 'GET, OPTIONS',
  'Access-Control-Expose-Headers': 'Content-Length, X-Cache',
};
const SPLIT = /^\/tracker\/download\/\d{6,9}\/(base|config\.[A-Za-z0-9_]+)$/;

export default {
  async fetch(req, env, ctx) {
    if (req.method === 'OPTIONS') return new Response(null, { headers: CORS });
    const { pathname } = new URL(req.url);
    const isSplit = SPLIT.test(pathname);
    if (req.method !== 'GET' || !(isSplit || pathname === '/tracker/index'))
      return new Response('not found', { status: 404, headers: CORS });

    const cache = caches.default;
    const key = new Request(`https://phoenix-apk-proxy.cache${pathname}`);
    const hit = await cache.match(key);
    if (hit) return withHeaders(hit, 'HIT');

    const up = await fetch(UPSTREAM + pathname, { redirect: 'follow' });
    const headers = new Headers({ 'Content-Type': up.headers.get('Content-Type') || 'application/octet-stream' });
    if (up.headers.get('Content-Length')) headers.set('Content-Length', up.headers.get('Content-Length'));
    // 404s (e.g. a density the tracker doesn't mirror) are cached briefly too
    headers.set('Cache-Control', up.ok && isSplit ? 'public, max-age=31536000, immutable' : 'public, max-age=300');
    const res = new Response(up.body, { status: up.status, headers });
    if (up.ok || up.status === 404) ctx.waitUntil(cache.put(key, res.clone()));
    return withHeaders(res, 'MISS');
  },
};

function withHeaders(res, state) {
  const h = new Headers(res.headers);
  for (const [k, v] of Object.entries(CORS)) h.set(k, v);
  h.set('X-Cache', state);
  return new Response(res.body, { status: res.status, headers: h });
}
