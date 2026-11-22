"""Add the gate.probe hook (classes2.dex + loader .so) to a Valenced Xposed module APK.

usage: python inject.py <valenced.apk> <classes.dex> <libgateprobe.so> <out.apk>
"""
import sys
import zipfile

src_apk, dex, so, out = sys.argv[1:5]
ENTRY = 'gate.probe.Probe'

with zipfile.ZipFile(src_apk) as src, zipfile.ZipFile(out, 'w') as dst:
    names = set(src.namelist())
    assert 'classes2.dex' not in names, 'base APK already has classes2.dex; use a clean Valenced APK'
    for info in src.infolist():
        data = src.read(info.filename)
        if info.filename == 'assets/xposed_init':
            lines = data.decode().split()
            if ENTRY not in lines:
                lines.append(ENTRY)
            data = ('\n'.join(lines) + '\n').encode()
        dst.writestr(info, data, compress_type=info.compress_type)
    dst.write(dex, 'classes2.dex', compress_type=zipfile.ZIP_DEFLATED)
    # stored (uncompressed) so it can be mapped straight from the APK; zipalign -p pages it
    dst.write(so, 'lib/x86_64/libgateprobe.so', compress_type=zipfile.ZIP_STORED)
print('wrote', out)
