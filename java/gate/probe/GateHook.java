package gate.probe;
import android.content.ContextWrapper;
import de.robv.android.xposed.*;

public class GateHook {
    public static void install(final ClassLoader cl) {
        // Launch forcing only. The access gate itself is fixed natively (gatepayload.so).
        try {
            XposedHelpers.findAndHookMethod("com.discord.phoenix_launch.PhoenixLaunch", cl, "i",
                ContextWrapper.class, new XC_MethodHook() {
                    @Override
                    protected void beforeHookedMethod(MethodHookParam p) {
                        p.setResult(Boolean.TRUE);
                    }
                });
            XposedBridge.log("GateProbe: PhoenixLaunch.i hook installed");
        } catch (Throwable t) {
            XposedBridge.log("GateProbe: PhoenixLaunch.i hook failed " + t);
        }
    }
}
