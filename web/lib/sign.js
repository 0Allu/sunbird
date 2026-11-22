// APK Signature Scheme v2 with WebCrypto (RSA-2048, PKCS#1 v1.5, SHA-256; algorithm 0x0103).
// Works on the part list from zip.writeZip: the signing block goes in front of the central directory.

const ALG_RSA_PKCS1_SHA256 = 0x0103;
const V2_BLOCK_ID = 0x7109871a;
const CHUNK = 1 << 20;
const subtle = globalThis.crypto.subtle;

// The one signing identity is web/signing-key.json ({pkcs8, spki, cert} base64, RSA-2048).
const ALGO = { name: 'RSASSA-PKCS1-v1_5', hash: 'SHA-256' };
const cat = (...a) => { const n = a.reduce((s, x) => s + x.length, 0), o = new Uint8Array(n); let p = 0; for (const x of a) { o.set(x, p); p += x.length; } return o; };

const u32 = (n) => { const b = new Uint8Array(4); new DataView(b.buffer).setUint32(0, n, true); return b; };
const u64 = (n) => { const b = new Uint8Array(8); new DataView(b.buffer).setBigUint64(0, BigInt(n), true); return b; };
const lp = (b) => cat(u32(b.length), b);

// 1 MiB chunks of a byte range spread over parts, chunk boundaries never cross sections
function* chunksOf(parts) {
  const buf = new Uint8Array(CHUNK);
  let fill = 0;
  for (const p of parts) {
    let o = 0;
    while (o < p.length) {
      const n = Math.min(CHUNK - fill, p.length - o);
      buf.set(p.subarray(o, o + n), fill); fill += n; o += n;
      if (fill === CHUNK) { yield buf.slice(); fill = 0; }
    }
  }
  if (fill) yield buf.slice(0, fill);
}

async function contentDigest(sections) {
  const digests = [];
  for (const sec of sections) {
    for (const c of chunksOf(sec)) {
      digests.push(new Uint8Array(await subtle.digest('SHA-256', cat(new Uint8Array([0xa5]), u32(c.length), c))));
    }
  }
  return new Uint8Array(await subtle.digest('SHA-256', cat(new Uint8Array([0x5a]), u32(digests.length), ...digests)));
}

/** Sign {parts, cdOffset, cdSize} from writeZip. Returns the new parts array (a Blob-able APK). */
export async function signV2(zip, id, onProgress = () => {}) {
  const { parts, cdOffset } = zip;
  // split parts into: entries | central directory | EOCD (last part)
  const eocd = parts[parts.length - 1];
  const entries = [], cd = [];
  let pos = 0;
  for (const p of parts.slice(0, -1)) { (pos < cdOffset ? entries : cd).push(p); pos += p.length; }
  onProgress('hashing');
  const digest = await contentDigest([entries, cd, [eocd]]);
  const signedData = cat(
    lp(lp(cat(u32(ALG_RSA_PKCS1_SHA256), lp(digest)))),
    lp(lp(id.cert)),
    lp(new Uint8Array(0)),
  );
  const key = await subtle.importKey('pkcs8', id.pkcs8, ALGO, false, ['sign']);
  const sig = new Uint8Array(await subtle.sign('RSASSA-PKCS1-v1_5', key, signedData));
  const signer = cat(lp(signedData), lp(lp(cat(u32(ALG_RSA_PKCS1_SHA256), lp(sig)))), lp(id.spki));
  const value = lp(lp(signer));
  const pair = cat(u64(4 + value.length), u32(V2_BLOCK_ID), value);
  const magic = new TextEncoder().encode('APK Sig Block 42');
  const blockSize = pair.length + 8 + magic.length;
  const block = cat(u64(blockSize), pair, u64(blockSize), magic);
  const newEocd = eocd.slice();
  new DataView(newEocd.buffer).setUint32(16, cdOffset + block.length, true);
  return [...entries, block, ...cd, newEocd];
}
