//! phoenix.native: symbols, memory, calls and inline hooks (ShadowHook, arm64). Addresses and
//! register values are BigInts (arm64 heap pointers carry tag bits above 2^53); numbers work too.
use hook::inline;
use hook::module::Module;
use rquickjs::{function::Rest, BigInt, Ctx, Exception, Function, Object, Result, TypedArray, Value};
use std::ffi::c_void;

fn u64_of(v: &Value) -> Option<u64> {
    if let Some(b) = v.as_big_int() { return b.clone().to_i64().ok().map(|x| x as u64); }
    v.as_number().map(|n| n as i64 as u64)
}
fn addr(ctx: &Ctx, v: Value) -> Result<u64> {
    u64_of(&v).ok_or_else(|| Exception::throw_type(ctx, "expected an address (BigInt or number)"))
}
fn big<'js>(ctx: &Ctx<'js>, v: u64) -> Result<Value<'js>> { Ok(BigInt::from_u64(ctx.clone(), v)?.into_value()) }

#[repr(C)]
struct Iovec { base: *mut c_void, len: usize }
extern "C" {
    fn process_vm_readv(pid: i32, l: *const Iovec, ln: u64, r: *const Iovec, rn: u64, flags: u64) -> isize;
    fn process_vm_writev(pid: i32, l: *const Iovec, ln: u64, r: *const Iovec, rn: u64, flags: u64) -> isize;
    fn getpid() -> i32;
}
/// Copies through the kernel: a bad address is an error, not a crash.
fn rw(write: bool, at: u64, buf: &mut [u8]) -> bool {
    let l = Iovec { base: buf.as_mut_ptr() as *mut c_void, len: buf.len() };
    let r = Iovec { base: at as *mut c_void, len: buf.len() };
    let n = unsafe { if write { process_vm_writev(getpid(), &l, 1, &r, 1, 0) } else { process_vm_readv(getpid(), &l, 1, &r, 1, 0) } };
    n == buf.len() as isize
}

/// ShadowHook calls this on whatever thread hits the hooked code.
unsafe extern "C" fn on_intercept(cpu: *mut inline::CpuContext, data: *mut c_void) {
    let cpu = &mut *cpu;
    crate::with(|ctx| -> Option<()> {
        let regs = cpu.regs.iter().map(|&r| big(&ctx, r)).collect::<Result<Vec<Value>>>().ok()?;
        let (sp, pc) = (big(&ctx, cpu.sp).ok()?, big(&ctx, cpu.pc).ok()?);
        let out: Option<Vec<Value>> = crate::call(&ctx, "__phx_intercept", (data as usize as f64, regs, sp, pc));
        for (i, v) in out.unwrap_or_default().iter().enumerate().take(31) {
            if let Some(x) = u64_of(v) { cpu.regs[i] = x; }
        }
        Some(())
    });
}

pub fn install<'js>(ctx: &Ctx<'js>, p: &Object<'js>) -> Result<()> {
    let n = Object::new(ctx.clone())?;
    let host = Module::find("liblibdiscord_wasm_host.so");
    n.set("base", big(ctx, host.map_or(0, |m| m.base as u64))?)?;
    n.set("arch", if cfg!(target_arch = "aarch64") { "arm64" } else { "x86_64" })?;
    n.set("inlineHooks", inline::AVAILABLE)?;
    // sym(name, lib = host lib): exported symbol address, or null
    n.set("sym", Function::new(ctx.clone(), |ctx: Ctx<'js>, name: String, lib: rquickjs::function::Opt<String>| -> Result<Value<'js>> {
        let m = Module::find(lib.0.as_deref().unwrap_or("liblibdiscord_wasm_host.so"));
        match m.and_then(|m| m.dlsym(&name)) { Some(a) => big(&ctx, a as u64), None => Ok(Value::new_null(ctx)) }
    })?)?;
    n.set("moduleBase", Function::new(ctx.clone(), |ctx: Ctx<'js>, lib: String| -> Result<Value<'js>> {
        match Module::find(&lib) { Some(m) => big(&ctx, m.base as u64), None => Ok(Value::new_null(ctx)) }
    })?)?;
    n.set("read", Function::new(ctx.clone(), |ctx: Ctx<'js>, at: Value<'js>, len: usize| -> Result<TypedArray<'js, u8>> {
        let at = addr(&ctx, at)?;
        let mut b = vec![0u8; len.min(64 << 20)];
        if !rw(false, at, &mut b) { return Err(Exception::throw_range(&ctx, &format!("cannot read {len} bytes at 0x{at:x}"))); }
        TypedArray::new(ctx, b)
    })?)?;
    n.set("write", Function::new(ctx.clone(), |ctx: Ctx<'js>, at: Value<'js>, data: TypedArray<'js, u8>| -> Result<bool> {
        let at = addr(&ctx, at)?;
        let mut b = data.as_raw().map(|p| unsafe { p.as_ref() }.to_vec()).unwrap_or_default();
        Ok(rw(true, at, &mut b))
    })?)?;
    // call(addr, ...args): up to 8 integer/pointer args, integer result (no floats, no structs)
    n.set("call", Function::new(ctx.clone(), |ctx: Ctx<'js>, at: Value<'js>, args: Rest<Value<'js>>| -> Result<Value<'js>> {
        let at = addr(&ctx, at)?;
        if args.0.len() > 8 { return Err(Exception::throw_range(&ctx, "native.call takes at most 8 arguments")); }
        let mut a = [0u64; 8];
        for (i, v) in args.0.iter().enumerate() { a[i] = u64_of(v).ok_or_else(|| Exception::throw_type(&ctx, "arguments must be integers"))?; }
        let f: extern "C" fn(u64, u64, u64, u64, u64, u64, u64, u64) -> u64 = unsafe { std::mem::transmute(at as usize) };
        big(&ctx, f(a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7]))
    })?)?;
    // intercept(addr, id, isFunc) -> stub; the JS side keeps the callback under `id`
    n.set("intercept", Function::new(ctx.clone(), |ctx: Ctx<'js>, at: Value<'js>, id: u32, func: bool| -> Result<Value<'js>> {
        let at = addr(&ctx, at)?;
        match inline::intercept(at as usize, func, on_intercept, id as usize) {
            Ok(stub) => big(&ctx, stub as u64),
            Err(e) => Err(Exception::throw_message(&ctx, &e)),
        }
    })?)?;
    n.set("unintercept", Function::new(ctx.clone(), |ctx: Ctx<'js>, stub: Value<'js>| -> Result<bool> {
        let stub = addr(&ctx, stub)?;
        inline::unintercept(stub as usize).map(|_| true).map_err(|e| Exception::throw_message(&ctx, &e))
    })?)?;
    p.set("native", n)
}
