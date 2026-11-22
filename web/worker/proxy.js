// Cloudflare Worker: CORS pass-through for the Discord APK tracker, which redirects to Google's
// CDN (no CORS headers). Only /tracker/index and /tracker/download/<versionCode>/<split> are allowed.
// Deploy: `npx wrangler deploy web/worker/proxy.js --name phoenix-apk-proxy` and put the URL in
// web/config.js (PROXY).
const UPSTREAM = 'https://tracker.vendetta.rocks';
const CORS = {
  'Access-Control-Allow-Origin': '*',
  'Access-Control-Allow-Methods': 'GET, OPTIONS',
  'Access-Control-Expose-Headers': 'Content-Length',
};

export default {
  async fetch(req) {
    if (req.method === 'OPTIONS') return new Response(null, { headers: CORS });
    const { pathname } = new URL(req.url);
    if (req.method !== 'GET' || !/^\/tracker\/(index|download\/\d{6,9}\/(base|config\.[A-Za-z0-9_]+))$/.test(pathname))
      return new Response('not found', { status: 404, headers: CORS });
    const up = await fetch(UPSTREAM + pathname, { redirect: 'follow' });
    const headers = new Headers(CORS);
    for (const h of ['Content-Type', 'Content-Length']) if (up.headers.get(h)) headers.set(h, up.headers.get(h));
    return new Response(up.body, { status: up.status, headers });
  },
};
