// Phoenix plugin API v2 (libplg's rquickjs runtime). A plugin is a plain script run with `phoenix`
// and a tagged `console` in scope; bundle TypeScript with esbuild (see plugins/example).
// Reference: plugin/API.md.
//
// Plugin header (leading comment lines):
//   // @name MyPlugin
//   // @description What it does
//   // @version 1.0.0
//   // @author you
//   // @api 2            (refused cleanly by apps with an older API)

declare global {
  // ---- gateway payloads (the fields plugins usually need; Discord sends more) -------------------
  interface User { id: string; username: string; global_name?: string | null; discriminator?: string; avatar?: string | null; bot?: boolean }
  interface Message {
    id: string; channel_id: string; guild_id?: string; content: string; author: User;
    timestamp: string; edited_timestamp?: string | null; type: number; flags?: number;
    attachments: any[]; embeds: any[]; mentions: User[]; [k: string]: any;
  }
  interface GatewayEvents {
    READY: { user: User; [k: string]: any };
    MESSAGE_CREATE: Message;
    MESSAGE_UPDATE: Partial<Message> & { id: string; channel_id: string };
    MESSAGE_DELETE: { id: string; channel_id: string; guild_id?: string };
    MESSAGE_DELETE_BULK: { ids: string[]; channel_id: string; guild_id?: string };
    MESSAGE_REACTION_ADD: { user_id: string; channel_id: string; message_id: string; guild_id?: string; emoji: { id: string | null; name: string } };
    MESSAGE_REACTION_REMOVE: { user_id: string; channel_id: string; message_id: string; guild_id?: string; emoji: { id: string | null; name: string } };
    TYPING_START: { user_id: string; channel_id: string; guild_id?: string; timestamp: number };
    PRESENCE_UPDATE: { user: { id: string }; status: string; guild_id?: string; activities: any[] };
    CHANNEL_CREATE: { id: string; type: number; guild_id?: string; name?: string };
    GUILD_MEMBER_UPDATE: { guild_id: string; user: User; nick?: string | null; roles: string[] };
    [event: string]: any;
  }

  /** A gateway event as Phoenix's handler sees it. `data` is parsed lazily from `raw`. */
  interface PhoenixEvent<T = any> { readonly name: string; raw: string; readonly data: T }
  /**
   * Runs on Phoenix's store thread before its own handler: return false to drop the event, an
   * object or string to replace its JSON, nothing to pass it on.
   */
  type SyncListener<T = any> = (ev: PhoenixEvent<T>) => false | object | string | void;
  /** Bridge listener: false drops the frame, a Uint8Array replaces it. */
  type BridgeListener = (frame: Uint8Array) => false | Uint8Array | void;

  interface PhoenixResponse { status: number; ok: boolean; text(): Promise<string>; json<T = any>(): Promise<T> }
  interface PhoenixStorage { get<T = any>(key: string, fallback?: T): T; set(key: string, value: unknown): void; delete(key: string): void }

  type PhoenixSetting =
    | { type: 'switch'; title: string; description?: string; default?: boolean }
    | { type: 'text'; title: string; description?: string; default?: string }
    | { type: 'number'; title: string; description?: string; default?: number }
    | { type: 'button'; title: string; onPress: () => void };

  /** A remover; update() re-renders this contribution everywhere it is shown. */
  interface Contribution { (): void; update(): void }
  /** Phoenix drawable, e.g. "res://ic_mana_puzzle_piece_plus". */
  type Icon = string;
  interface UiContext { channelId: string | null; /** Local "only you can see this" message from Phoenix. */ reply(text: string): Promise<string> }
  type UiText = string | { text: string; color?: number } | null | undefined | false;
  interface CommandContext { channelId: string | null; reply(text: string): Promise<string> }

  interface PluginInfo { id: string; name: string; description: string; version: string; author: string; enabled: boolean; error: string | null; origin: string }

  interface Phoenix {
    /** This plugin's id (file name without .js). */
    readonly id: string;
    readonly version: { api: number; runtime: string; arch: 'arm64' | 'x86_64'; inlineHooks: boolean };
    /** Every gateway event Phoenix handles in this build. */
    readonly events: readonly string[];
    /** Resolves once Phoenix has handled READY (logged in, stores filled). */
    readonly ready: Promise<void>;

    // ---- gateway -------------------------------------------------------------------------------
    /** Sync listener on Phoenix's store thread. Returns an unsubscribe function. */
    on<K extends keyof GatewayEvents>(event: K, fn: SyncListener<GatewayEvents[K]>): () => void;
    /** Async listener on the JS thread; never blocks Phoenix. */
    observe<K extends keyof GatewayEvents>(event: K, fn: (ev: PhoenixEvent<GatewayEvents[K]>) => void): () => void;
    /** Runs an event through Phoenix's own handler (store thread). Resolves to whether it was accepted. */
    dispatch<K extends keyof GatewayEvents>(event: K, data: GatewayEvents[K] | string): Promise<boolean | null>;

    // ---- app -----------------------------------------------------------------------------------
    /** Local-only changes: what this device shows. Nothing is sent to Discord. */
    readonly messages: {
      /** A message from "Phoenix" (or opts.author) in a channel. Resolves to its id. */
      addLocal(channelId: string, content: string, opts?: { author?: string; guildId?: string; embeds?: any[] }): Promise<string>;
      updateLocal(message: Partial<Message> & { id: string; channel_id: string }): Promise<boolean | null>;
      deleteLocal(channelId: string, messageId: string): Promise<boolean | null>;
      /** Before a message from the message box is sent: a string replaces it, false cancels. Synchronous. */
      onSend(fn: (text: string, ctx: CommandContext) => string | false | void): () => void;
    };
    /**
     * Phoenix's stores: its own database (files/store/<account>/cache.sqlite, read-only) plus
     * live reads on its store thread. Tables are key/value with JSON values; ids in keys are
     * zero-padded to 20 digits ("<guild>:<channel>", "<channel>:<message>", ...).
     */
    readonly stores: {
      /** The signed-in user (live serializer, else the persisted copy). */
      me(): Promise<any | null>;
      user(id: string): Promise<User | null>;
      users(opts?: { limit?: number }): Promise<User[]>;
      /** {id, properties (name, icon, ...), roles, emojis, stickers, member_count, joined_at_ms} */
      guilds(opts?: { limit?: number }): Promise<any[]>;
      guild(id: string): Promise<any | null>;
      /** Members Phoenix has seen in a guild (not the full list). */
      members(guildId: string, opts?: { limit?: number }): Promise<any[]>;
      member(guildId: string, userId: string): Promise<any | null>;
      /** Your notification settings for a guild. */
      guildSettings(guildId: string): Promise<any | null>;
      channel(id: string): Promise<any | null>;
      /** A guild's channels; guildId 0/null for DMs and group DMs. */
      channels(guildId: string | null, opts?: { limit?: number }): Promise<any[]>;
      privateChannels(opts?: { limit?: number }): Promise<any[]>;
      /** Cached messages, newest first. */
      messages(channelId: string, opts?: { limit?: number; before?: string }): Promise<Message[]>;
      message(channelId: string, id: string): Promise<Message | null>;
      readState(channelId: string): Promise<any | null>;
      relationships(): Promise<any[]>;
      /** A key of the meta table: me, guild_order, experiments, my_members, selected, ... */
      meta(key: string): Promise<any>;
      /** A persisted domain: profiles, threads, scheduled_events, collectibles, connections, user_affinities, ... */
      domain(name: string): Promise<any>;
      /** Every table of the database. */
      tables(): Promise<string[]>;
      get(table: string, key: string): Promise<any>;
      /** [{key, value}] ordered by key. */
      scan(table: string, opts?: { prefix?: string; after?: string; before?: string; reverse?: boolean; limit?: number }): Promise<{ key: string; value: any }[]>;
      count(table: string, opts?: { prefix?: string }): Promise<number>;
      /** Read-only SQL on the database. */
      sql(query: string, args?: (string | number)[], limit?: number): Promise<{ cols: string[]; rows: any[][]; more: boolean }>;
      /** Any serializer at host-lib offset `ser` on the store field at `off` (per-build offsets). */
      raw(ser: number, off: number): Promise<any | null>;
      /** Several reads in one go, synchronously, on the store thread. */
      read<T>(fn: (s: { me(): any | null; raw(ser: number, off: number): any | null }) => T): Promise<T>;
    };
    readonly ui: {
      toast(text: string, opts?: { long?: boolean }): void;
      /** A dialog with OK / Cancel (or opts.ok / opts.cancel). Resolves to true for OK. */
      confirm(title: string, message?: string, opts?: { ok?: string; cancel?: string }): Promise<boolean>;
      /** Android's document picker. Resolves to the file's name and text, or null if cancelled (4 MB max). */
      pickFile(mime?: string): Promise<{ name: string; text: string } | null>;
      /** Icon button in the chat header, left of search. */
      addHeaderButton(o: { id?: string; icon: Icon; label: string; priority?: number; onPress(ctx: UiContext): void }): Contribution;
      /** Icon button in the message box, left of the emoji button. */
      addComposerButton(o: { id?: string; icon: Icon; label: string; priority?: number; onPress(ctx: UiContext): void }): Contribution;
      /** Text under each message's content, or after its timestamp with placement 'header'. Runs per row: keep it fast. */
      addMessageAccessory(o: { id?: string; placement?: 'content' | 'header'; priority?: number; render(ctx: { messageId: string; channelId: string | null }): UiText | Promise<UiText> }): Contribution;
      /** Row in the message long-press menu (above Delete); the menu closes on press. */
      addMessageAction(o: { id?: string; label: string; icon?: Icon; danger?: boolean; priority?: number; visible?(ctx: { messageId: string | null; channelId: string | null }): boolean;
        onPress(ctx: UiContext & { messageId: string | null; message: Message | null }): void }): Contribution;
      /** Section at the end of a profile's Main tab. userId comes from the avatar (null for default avatars). */
      addProfileSection(o: { id?: string; title: string; priority?: number; render(ctx: { userId: string | null }): UiText | Promise<UiText> }): Contribution;
    };
    /** Slash commands typed in the message box. */
    readonly commands: {
      /** execute's returned text is sent instead; nothing returned (or a Promise) sends nothing. */
      register(o: { name: string; description?: string; execute(args: string, ctx: CommandContext): string | { content: string } | void | Promise<unknown> }): () => void;
      list(): { name: string; description: string; plugin: string }[];
    };
    /** HTTP through Android's HttpURLConnection. */
    fetch(url: string, init?: { method?: string; headers?: Record<string, string>; body?: string }): Promise<PhoenixResponse>;

    // ---- plugin --------------------------------------------------------------------------------
    /** Per-plugin JSON storage (files/phoenix-js/storage.json). */
    readonly storage: PhoenixStorage;
    /** Settings shown on the plugin's page in Settings → Phoenix, saved per plugin. */
    readonly settings: {
      /** Declare them each time the plugin starts; get() falls back to `default`. */
      define(defs: Record<string, PhoenixSetting>): void;
      get<T = any>(key: string): T;
      set(key: string, value: unknown): void;
      /** fn(key, value) after a setting changes (also from Settings). Removed when the plugin is turned off. */
      onChange(fn: (key: string, value: any) => void): () => void;
    };
    /** Runs when the plugin is turned off or reloaded (undo timers, Java hooks, ...). */
    onUnload(fn: () => void): void;
    /** What this plugin offers others: they read it with phoenix.plugins.get(id). */
    exports: Record<string, any>;
    readonly plugins: {
      /** Another plugin's exports, if it is installed, on and loaded. */
      get<T = Record<string, any>>(id: string): T | undefined;
      list(): PluginInfo[];
    };

    // ---- low level -----------------------------------------------------------------------------
    /** Native access. Addresses and register values are BigInts (arm64 pointers carry tag bits above 2^53). */
    readonly native: {
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
       * Runs `fn` before the function at `addr` (ShadowHook intercept) on the calling thread.
       * Change ctx.x[i] to change arguments; with {instruction: true} it hooks one instruction
       * (e.g. a `ret`, where x0 is the result). Removed when the plugin is turned off.
       */
      intercept(addr: bigint | number, fn: (ctx: { x: bigint[]; sp: bigint; pc: bigint }) => void, opts?: { instruction?: boolean }): () => void;
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
  /** Goes to logcat (tag PhoenixJS) and Settings → Phoenix → Console, prefixed with the plugin id. */
  const console: Record<'log' | 'info' | 'warn' | 'error' | 'debug', (...args: unknown[]) => void>;
  function setTimeout(fn: (...a: any[]) => void, ms?: number, ...args: any[]): number;
  function setInterval(fn: (...a: any[]) => void, ms?: number, ...args: any[]): number;
  function clearTimeout(id: number): void;
  function clearInterval(id: number): void;
}

export {};
