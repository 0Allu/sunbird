"""Patch a set of Discord split APKs (base + config.* splits) and re-sign them.

usage: python patch.py --ks key.jks --pass PASS --out OUTDIR base.apk config.arm64_v8a.apk [config.*.apk ...]

Then: adb install-multiple OUTDIR/*.apk   (uninstall the Play-signed com.discord first)
"""
import argparse
import os
import re
import subprocess
import sys
import zipfile

from dexpatch import patch_dex
from sopatch import patch_so

HOST_LIB = 'liblibdiscord_wasm_host.so'
SDK = os.environ.get('ANDROID_SDK', os.path.join(os.environ.get('LOCALAPPDATA', ''), 'Android', 'Sdk'))
BT = os.environ.get('BUILD_TOOLS', os.path.join(SDK, 'build-tools', '36.0.0'))
NDK_BIN = os.environ.get('NDK_BIN') or os.path.join(
    os.environ.get('ANDROID_NDK', os.path.join(SDK, 'ndk', '26.0.10792818')),
    'toolchains', 'llvm', 'prebuilt', 'windows-x86_64' if os.name == 'nt' else 'linux-x86_64', 'bin')
EXE = '.bat' if os.name == 'nt' else ''
SIG = re.compile(r'META-INF/([^/]+\.(SF|RSA|DSA|EC)|MANIFEST\.MF)$')


def rewrite(src, dst, edit):
    """Copy an APK, applying edit(name, data) -> data|None, dropping v1 signature files."""
    changed = []
    with zipfile.ZipFile(src) as zi, zipfile.ZipFile(dst, 'w') as zo:
        for info in zi.infolist():
            if SIG.match(info.filename):
                continue
            data = zi.read(info.filename)
            new = edit(info.filename, data)
            if new is not None:
                data = new
                changed.append(info.filename)
            zo.writestr(info, data, compress_type=info.compress_type)
    return changed


def align_sign(unsigned, out, ks, pw):
    aligned = unsigned + '.aligned'
    subprocess.run([os.path.join(BT, 'zipalign' + ('.exe' if os.name == 'nt' else '')), '-p', '-f', '4', unsigned, aligned], check=True)
    subprocess.run([os.path.join(BT, 'apksigner' + EXE), 'sign', '--ks', ks, '--ks-pass', f'pass:{pw}',
                    '--key-pass', f'pass:{pw}', '--out', out, aligned], check=True)
    os.remove(unsigned); os.remove(aligned)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--ks', required=True); ap.add_argument('--pass', dest='pw', required=True)
    ap.add_argument('--out', required=True); ap.add_argument('apks', nargs='+')
    a = ap.parse_args()
    os.makedirs(a.out, exist_ok=True)
    did = {'dex': False, 'so': []}

    def edit(name, data):
        if re.fullmatch(r'classes\d*\.dex', name) and not did['dex']:
            r = patch_dex(data)
            if r:
                did['dex'] = True
                print(f'{name}: PhoenixLaunch.i -> return true (code @ {r[1]:#x})')
                return r[0]
        if name.startswith('lib/') and name.endswith('/' + HOST_LIB):
            out, info = patch_so(data, NDK_BIN)
            did['so'].append(info['arch'])
            return out
        return None

    for apk in a.apks:
        name = os.path.basename(apk)
        tmp = os.path.join(a.out, name + '.tmp')
        rewrite(apk, tmp, edit)
        align_sign(tmp, os.path.join(a.out, name), a.ks, a.pw)
        print(f'signed {name}')
    if not did['dex'] or not did['so']:
        sys.exit(f'incomplete: dex patched={did["dex"]}, so patched for {did["so"]} (pass base.apk and the ABI split)')
    print(f'\nadb install-multiple {" ".join(os.path.join(a.out, os.path.basename(x)) for x in a.apks)}')


if __name__ == '__main__':
    main()
