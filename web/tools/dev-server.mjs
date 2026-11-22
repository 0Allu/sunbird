// Local test server: serves web/ and stands in for the proxy using split files from a folder.
// usage: node web/tools/dev-server.mjs <dir with base.apk, config.*.apk> [port]
// open http://localhost:8787/?proxy=http://localhost:8787 ; POST /upload saves a body to upload.apk
import { createReadStream, createWriteStream, existsSync, statSync } from 'node:fs';
import { createServer } from 'node:http';
import { extname, join, normalize, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = fileURLToPath(new URL('..', import.meta.url));
const [splitArg, port = 8787] = process.argv.slice(2);
const splits = resolve(splitArg);
const types = { '.html': 'text/html', '.js': 'text/javascript', '.apk': 'application/vnd.android.package-archive' };

createServer((req, res) => {
  const url = new URL(req.url, 'http://x');
  res.setHeader('Access-Control-Allow-Origin', '*');
  res.setHeader('Access-Control-Expose-Headers', 'Content-Length');
  if (req.method === 'POST' && url.pathname === '/upload') {
    req.pipe(createWriteStream('upload.apk')).on('finish', () => res.end('ok'));
    return;
  }
  let file;
  const m = url.pathname.match(/^\/tracker\/download\/\d+\/([\w.]+)$/);
  if (m) file = join(splits, `${m[1]}.apk`);
  else file = join(root, normalize(url.pathname === '/' ? '/index.html' : url.pathname));
  if (!file.startsWith(m ? splits : root) || !existsSync(file) || statSync(file).isDirectory()) { res.statusCode = 404; return res.end('not found'); }
  res.setHeader('Content-Type', types[extname(file)] || 'application/octet-stream');
  res.setHeader('Content-Length', statSync(file).size);
  createReadStream(file).pipe(res);
}).listen(port, () => console.log(`http://localhost:${port}/?proxy=http://localhost:${port}`));
