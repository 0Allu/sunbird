// APK Signature Scheme v3 with WebCrypto (RSA PKCS#1 v1.5), signing like crates/patcher's apksigner call
// (`--v1/--v2-signing-enabled false --lineage lineage --rotation-min-sdk-version 28`): Phoenix Unlock's key
// (phoenix.pk8, CN=phoenix-unlock) with the proof of rotation from the key earlier builds used, so installs signed
// with either update in place. v3 only, since v1/v2 would need the old private key: Android 9+.
// Works on the part list from zip.writeZip: the signing block goes in front of the central directory.
//
// Keys: web/signing-key.json = crates/patcher/keys as base64 ({new: {pkcs8, spki, cert}, lineage}),
// lineage being the proof-of-rotation attribute (the lineage file without its 12-byte header).

const V3_BLOCK_ID = 0xf05368c0;
const PROOF_OF_ROTATION = 0x3ba06f8c;
const ROTATION_MIN_SDK = 28;
const MAX_SDK = 0x7fffffff;
const CHUNK = 1 << 20;
const subtle = globalThis.crypto.subtle;
// apksigner signs with SHA-512 for RSA keys over 3072 bits
const DIGESTS = { 'SHA-256': 0x0103, 'SHA-512': 0x0104 };

const cat = (...a) => { const n = a.reduce((s, x) => s + x.length, 0), o = new Uint8Array(n); let p = 0; for (const x of a) { o.set(x, p); p += x.length; } return o; };
const u32 = (n) => { const b = new Uint8Array(4); new DataView(b.buffer).setUint32(0, n, true); return b; };
const u64 = (n) => { const b = new Uint8Array(8); new DataView(b.buffer).setBigUint64(0, BigInt(n), true); return b; };
const lp = (b) => cat(u32(b.length), b);
const unb64 = (s) => Uint8Array.from(atob(s), (c) => c.charCodeAt(0));

/** signing-key.json -> keys for signApk */
export function keysFrom(json) {
  const key = (k) => ({ pkcs8: unb64(k.pkcs8), spki: unb64(k.spki), cert: unb64(k.cert) });
  return { new: key(json.new), lineage: unb64(json.lineage) };
}

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

/** Chunked content digests, one per hash, in a single pass over the APK. */
async function contentDigests(sections, hashes) {
  const chunks = hashes.map(() => []);
  for (const sec of sections)
    for (const c of chunksOf(sec)) {
      const data = cat(new Uint8Array([0xa5]), u32(c.length), c);
      await Promise.all(hashes.map(async (h, i) => chunks[i].push(new Uint8Array(await subtle.digest(h, data)))));
    }
  return Promise.all(hashes.map(async (h, i) => new Uint8Array(await subtle.digest(h, cat(new Uint8Array([0x5a]), u32(chunks[i].length), ...chunks[i])))));
}

async function importKey(k) {
  const probe = await subtle.importKey('pkcs8', k.pkcs8, { name: 'RSASSA-PKCS1-v1_5', hash: 'SHA-256' }, false, ['sign']);
  const hash = probe.algorithm.modulusLength > 3072 ? 'SHA-512' : 'SHA-256';
  return { ...k, hash, key: hash === 'SHA-256' ? probe : await subtle.importKey('pkcs8', k.pkcs8, { name: 'RSASSA-PKCS1-v1_5', hash }, false, ['sign']) };
}

/** One signer: signed data (digest, cert, [sdk range], attributes), [sdk range], signature, public key. */
async function signer(k, digest, attrs, sdk) {
  const alg = DIGESTS[k.hash];
  const signedData = cat(lp(lp(cat(u32(alg), lp(digest)))), lp(lp(k.cert)), sdk, lp(attrs));
  const sig = new Uint8Array(await subtle.sign('RSASSA-PKCS1-v1_5', k.key, signedData));
  return lp(lp(cat(lp(signedData), sdk, lp(lp(cat(u32(alg), lp(sig)))), lp(k.spki))));
}

const pair = (id, value) => cat(u64(4 + value.length), u32(id), value);

/** Sign {parts, cdOffset} from writeZip with keysFrom(signing-key.json). Returns the new parts array (a Blob-able APK). */
export async function signApk(zip, keys, onProgress = () => {}) {
  const { parts, cdOffset } = zip;
  // split parts into: entries | central directory | EOCD (last part)
  const eocd = parts[parts.length - 1];
  const entries = [], cd = [];
  let pos = 0;
  for (const p of parts.slice(0, -1)) { (pos < cdOffset ? entries : cd).push(p); pos += p.length; }
  const newKey = await importKey(keys.new);
  onProgress('hashing');
  const [digest] = await contentDigests([entries, cd, [eocd]], [newKey.hash]);
  const v3 = await signer(newKey, digest, lp(cat(u32(PROOF_OF_ROTATION), keys.lineage)), cat(u32(ROTATION_MIN_SDK), u32(MAX_SDK)));
  const pairs = pair(V3_BLOCK_ID, v3);
  const magic = new TextEncoder().encode('APK Sig Block 42');
  const blockSize = pairs.length + 8 + magic.length;
  const block = cat(u64(blockSize), pairs, u64(blockSize), magic);
  const newEocd = eocd.slice();
  new DataView(newEocd.buffer).setUint32(16, cdOffset + block.length, true);
  return [...entries, block, ...cd, newEocd];
}
