// Types for Phoenix plugins (libplg JS runtime, rquickjs / QuickJS-ng). A plugin is a plain
// script that runs with `phoenix` and a tagged `console` in scope; bundle TypeScript with esbuild
// (see plugins/example).

declare global {
  /** A gateway event as Phoenix's handler sees it. `data` is parsed lazily from `raw`. */
  interface PhoenixEvent<T = any> {
    readonly name: string;
    raw: string;
    readonly data: T;
  }

  /**
   * Sync listener, run on Phoenix's store thread before its own handler:
   * `false` drops the event, an object or string replaces its JSON, nothing passes it on.
   */
  type SyncListener<T = any> = (ev: PhoenixEvent<T>) => false | object | string | void;

  /** Bridge listener: `false` drops the frame, a Uint8Array replaces it. */
  type BridgeListener = (frame: Uint8Array) => false | Uint8Array | void;

  interface PhoenixResponse {
    status: number;
    ok: boolean;
    text(): Promise<string>;
    json<T = any>(): Promise<T>;
  }

  interface PhoenixStorage {
    get<T = any>(key: string, fallback?: T): T;
    set(key: string, value: unknown): void;
    delete(key: string): void;
  }

  interface Phoenix {
    /** This plugin's id (file name without .js). */
    readonly id: string;
    /** Every gateway event Phoenix handles in this build. */
    readonly events: readonly string[];
    /** Sync listener (store thread). Returns an unsubscribe function. */
    on<T = any>(event: string, fn: SyncListener<T>): () => void;
    /** Async listener on the JS thread; never blocks Phoenix. */
    observe<T = any>(event: string, fn: (ev: PhoenixEvent<T>) => void): () => void;
    /** Runs an event through Phoenix's own handler, as if the gateway sent it. Call it from a sync listener. */
    dispatch(event: string, data: object | string): boolean | null;
    /** HTTP through Android's HttpURLConnection. */
    fetch(url: string, init?: { method?: string; headers?: Record<string, string>; body?: string }): Promise<PhoenixResponse>;
    /** Per-plugin JSON storage (files/phoenix-js/storage.json). */
    readonly storage: PhoenixStorage;
    /** Live Phoenix stores, read with Phoenix's own serializers. Only inside a sync listener. */
    readonly stores: {
      /** The signed-in user; null if this build's Me store was not found by the patcher. */
      me(): any | null;
      /** Any serializer at host-lib offset `ser` on the store field at `off` (per-build offsets). */
      raw(ser: number, off: number): any | null;
    };
    /**
     * Native access. Addresses and register values are BigInts (arm64 heap pointers carry tag
     * bits above 2^53); plain numbers are accepted as input.
     */
    readonly native: {
      /** Load address of liblibdiscord_wasm_host.so. */
      readonly base: bigint;
      readonly arch: 'arm64' | 'x86_64';
      /** true when intercept() works (ShadowHook, arm64 only). */
      readonly inlineHooks: boolean;
      /** Host-lib offset (e.g. from scan) to address. */
      at(offset: number | bigint): bigint;
      /** IDA-style pattern ("48 8B ?? 05") over the host lib's code; host-relative offsets (max 64). */
      scan(pattern: string): number[];
      /** Exported symbol of `lib` (default: the host lib), or null. */
      sym(name: string, lib?: string): bigint | null;
      moduleBase(lib: string): bigint | null;
      /** Reads through the kernel: a bad address throws instead of crashing. */
      read(addr: bigint | number, length: number): Uint8Array;
      write(addr: bigint | number, data: Uint8Array): boolean;
      readU64(addr: bigint | number): bigint;
      readCString(addr: bigint | number, max?: number): string;
      /** Calls a C function with up to 8 integer/pointer arguments; integer result. */
      call(addr: bigint | number, ...args: (bigint | number)[]): bigint;
      /**
       * Runs `fn` before the function at `addr` (ShadowHook intercept), on the calling thread.
       * Change ctx.x[i] to change arguments; with {instruction: true} it hooks one instruction
       * instead (e.g. a `ret`, where x0 is the result). Removed when the plugin is turned off.
       * Keep it cheap: it runs on every call, under the runtime lock.
       */
      intercept(addr: bigint | number, fn: (ctx: { x: bigint[]; sp: bigint; pc: bigint }) => void, opts?: { instruction?: boolean }): () => void;
    };
    /** Runs when the plugin is turned off in Settings → Phoenix. */
    onUnload(fn: () => void): void;
    /** The guest <-> host bridge: UI-op batches, guest->host invokes, host->guest dispatches. */
    readonly bridge: {
      onOps(fn: BridgeListener): () => void;
      onInvoke(fn: BridgeListener): () => void;
      onInvokeResult(fn: (request: Uint8Array, response: Uint8Array) => Uint8Array | void): () => void;
      onDispatch(fn: BridgeListener): () => void;
    };
  }

  const phoenix: Phoenix;
  /** Goes to logcat (tag PhoenixJS), prefixed with the plugin id. */
  const console: Record<'log' | 'info' | 'warn' | 'error' | 'debug', (...args: unknown[]) => void>;
  function setTimeout(fn: (...a: any[]) => void, ms?: number, ...args: any[]): number;
  function setInterval(fn: (...a: any[]) => void, ms?: number, ...args: any[]): number;
  function clearTimeout(id: number): void;
  function clearInterval(id: number): void;
}

export {};
