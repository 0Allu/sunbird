package de.robv.android.xposed;
public abstract class XC_MethodHook {
    public static class Unhook { public void unhook() {} }
    public static class MethodHookParam {
        public java.lang.reflect.Member method;
        public Object thisObject;
        public Object[] args;
        private Object result;
        public Object getResult() { return result; }
        public void setResult(Object r) { this.result = r; }
    }
    protected void beforeHookedMethod(MethodHookParam param) throws Throwable {}
    protected void afterHookedMethod(MethodHookParam param) throws Throwable {}
}
