package phx;

import android.app.Activity;
import android.app.Fragment;
import android.content.ContentResolver;
import android.content.Intent;
import android.database.Cursor;
import android.net.Uri;
import android.os.Bundle;
import android.os.Handler;
import android.os.Looper;
import android.provider.OpenableColumns;
import java.io.ByteArrayOutputStream;
import java.io.InputStream;
import java.lang.reflect.Field;
import java.nio.charset.StandardCharsets;

/**
 * Opens Android's document picker from a headless fragment and hands the chosen file's text to
 * native code (libplg loads this class from an in-memory dex and registers onPicked).
 */
public final class Picker extends Fragment {
    private static final int REQ = 0x5048;
    private static final int MAX = 4 << 20;

    /** name/text are null when nothing was picked; error is null unless something failed. */
    static native void onPicked(int id, String name, String text, String error);

    private int id;
    private String mime;

    public Picker() {} // fragments need a public no-arg constructor

    /** Finds the Activity held by `owner` (Phoenix's GuestHost) and opens the picker on the main thread. */
    public static void pick(final Object owner, final int id, final String mime) {
        new Handler(Looper.getMainLooper()).post(new Runnable() {
            public void run() {
                try {
                    Activity a = findActivity(owner);
                    if (a == null) { onPicked(id, null, null, "no activity"); return; }
                    Picker p = new Picker();
                    p.id = id;
                    p.mime = mime;
                    p.setRetainInstance(true); // survives rotation without being re-created by name
                    a.getFragmentManager().beginTransaction().add(p, "phx-picker-" + id).commitAllowingStateLoss();
                } catch (Throwable t) {
                    onPicked(id, null, null, String.valueOf(t));
                }
            }
        });
    }

    static Activity findActivity(Object owner) throws IllegalAccessException {
        for (Class<?> c = owner.getClass(); c != null; c = c.getSuperclass())
            for (Field f : c.getDeclaredFields())
                if (Activity.class.isAssignableFrom(f.getType())) {
                    f.setAccessible(true);
                    Object v = f.get(owner);
                    if (v != null) return (Activity) v;
                }
        return null;
    }

    @Override
    public void onCreate(Bundle state) {
        super.onCreate(state);
        if (state != null && mime == null) { done(); return; } // restored after process death: nobody waits
        Intent i = new Intent(Intent.ACTION_OPEN_DOCUMENT).addCategory(Intent.CATEGORY_OPENABLE).setType(mime == null ? "*/*" : mime);
        startActivityForResult(i, REQ);
    }

    @Override
    public void onActivityResult(int req, int res, Intent data) {
        if (req != REQ) return;
        final Uri uri = res == Activity.RESULT_OK && data != null ? data.getData() : null;
        final ContentResolver cr = getActivity() != null ? getActivity().getContentResolver() : null;
        final int id = this.id;
        done();
        if (uri == null || cr == null) { onPicked(id, null, null, null); return; }
        new Thread(new Runnable() {
            public void run() {
                try {
                    String name = null;
                    Cursor c = cr.query(uri, new String[] {OpenableColumns.DISPLAY_NAME}, null, null, null);
                    if (c != null) { if (c.moveToFirst()) name = c.getString(0); c.close(); }
                    InputStream in = cr.openInputStream(uri);
                    ByteArrayOutputStream out = new ByteArrayOutputStream();
                    byte[] buf = new byte[65536];
                    for (int n; (n = in.read(buf)) > 0; ) {
                        out.write(buf, 0, n);
                        if (out.size() > MAX) { in.close(); onPicked(id, name, null, "file is larger than 4 MB"); return; }
                    }
                    in.close();
                    onPicked(id, name == null ? "plugin.js" : name, new String(out.toByteArray(), StandardCharsets.UTF_8), null);
                } catch (Throwable t) {
                    onPicked(id, null, null, String.valueOf(t));
                }
            }
        }, "phoenix-picker").start();
    }

    private void done() {
        if (getFragmentManager() != null) getFragmentManager().beginTransaction().remove(this).commitAllowingStateLoss();
    }
}
