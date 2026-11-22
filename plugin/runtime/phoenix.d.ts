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
    readonly native: {
      /** IDA-style pattern ("48 8B ?? 05") over the host lib's code; host-relative offsets (max 64). */
      scan(pattern: string): number[];
    };
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
