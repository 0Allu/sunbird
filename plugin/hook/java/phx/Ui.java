package phx;

import android.app.Activity;
import android.app.AlertDialog;
import android.content.DialogInterface;
import android.os.Handler;
import android.os.Looper;
import android.widget.Toast;

/** Small Android UI for plugins: toasts and confirm dialogs, shown on Phoenix's activity. */
public final class Ui {
    private Ui() {}

    /** Result of confirm(): ok is false when cancelled or dismissed. */
    static native void onConfirm(int id, boolean ok);

    private static void main(Runnable r) { new Handler(Looper.getMainLooper()).post(r); }

    public static void toast(final Object owner, final String text, final boolean longer) {
        main(new Runnable() {
            public void run() {
                try {
                    Activity a = Picker.findActivity(owner);
                    if (a != null) Toast.makeText(a, text, longer ? Toast.LENGTH_LONG : Toast.LENGTH_SHORT).show();
                } catch (Throwable ignored) {
                }
            }
        });
    }

    public static void confirm(final Object owner, final int id, final String title, final String message, final String ok, final String cancel) {
        main(new Runnable() {
            public void run() {
                try {
                    Activity a = Picker.findActivity(owner);
                    if (a == null) { onConfirm(id, false); return; }
                    final boolean[] sent = {false};
                    final DialogInterface.OnClickListener click = new DialogInterface.OnClickListener() {
                        public void onClick(DialogInterface d, int which) {
                            if (!sent[0]) { sent[0] = true; onConfirm(id, which == DialogInterface.BUTTON_POSITIVE); }
                        }
                    };
                    new AlertDialog.Builder(a).setTitle(title).setMessage(message)
                        .setPositiveButton(ok, click).setNegativeButton(cancel, click)
                        .setOnDismissListener(new DialogInterface.OnDismissListener() {
                            public void onDismiss(DialogInterface d) {
                                if (!sent[0]) { sent[0] = true; onConfirm(id, false); }
                            }
                        }).show();
                } catch (Throwable t) {
                    onConfirm(id, false);
                }
            }
        });
    }
}
