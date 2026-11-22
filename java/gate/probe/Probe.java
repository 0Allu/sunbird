package gate.probe;
import de.robv.android.xposed.IXposedHookLoadPackage;
import de.robv.android.xposed.callbacks.XC_LoadPackage;
public class Probe implements IXposedHookLoadPackage {
    @Override
    public void handleLoadPackage(XC_LoadPackage.LoadPackageParam lpparam) {
        if (!"com.discord".equals(lpparam.packageName)) return;
        try {
            System.loadLibrary("gateprobe"); // loader: dlopens /data/local/tmp/gatepayload.so
            GateHook.install(lpparam.classLoader);
            android.util.Log.e("GateProbe", "library loaded");
        } catch (Throwable t) {
            android.util.Log.e("GateProbe", "hook failed", t);
        }
    }
}
