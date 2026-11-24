// Runs the patcher (crates/patcher-web, built to pkg/ by scripts/site.nu) off the page's thread. Splits arrive
// transferred and get copied into wasm memory once. The APK leaves as a Blob, built from views of that memory
// before anything else calls into wasm.
import init, { Build } from './pkg/patcher_web.js';

const ready = init();

onmessage = async ({ data: { apks, libraries, plugins, frida, packageName, label } }) => {
  await ready;
  const build = new Build();
  try {
    for (const apk of apks) build.addApk(apk);
    if (plugins) {
      for (const { name, bytes } of libraries) build.addLibrary(name, bytes);
      for (const { name, source } of plugins) build.addPlugin(name, source);
    }
    if (frida) {
      for (const { name, code } of frida.scripts) build.addFridaScript(name, code);
      build.setFrida(frida.gadget, frida.bootstrap);
    }
    const parts = build.merge(packageName, label, (line) => postMessage({ log: line }));
    postMessage({ apk: new Blob(parts, { type: 'application/vnd.android.package-archive' }) });
  } catch (e) {
    postMessage({ error: String(e?.message ?? e) });
  } finally {
    build.free();
  }
};
