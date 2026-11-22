// Types for the `plumage` object every plugin bundle receives.

declare namespace Plumage {
  /// Partial user structure, resources/user. `public_flags` is User Flags.
  interface User {
    id: string;
    username: string;
    discriminator: string;
    global_name?: string | null;
    avatar: string | null;
    avatar_decoration_data?: AvatarDecorationData | null;
    collectibles?: Collectibles | null;
    display_name_styles?: DisplayNameStyle | null;
    primary_guild?: PrimaryGuild | null;
    bot?: boolean;
    system?: boolean;
    banner?: string | null;
    accent_color?: number | null;
    public_flags?: number;
  }

  /// `expires_at` is a Unix timestamp, not ISO.
  interface AvatarDecorationData {
    asset: string;
    sku_id: string;
    expires_at: number | null;
  }

  interface Collectibles {
    nameplate: NameplateData | null;
  }

  interface NameplateData {
    asset: string;
    sku_id: string;
    label: string;
    palette: string;
    expires_at: number | null;
  }

  /// `font_id` is Display Name Font, `effect_id` is Display Name Effect.
  interface DisplayNameStyle {
    font_id: number;
    effect_id: number;
    colors: number[];
  }

  interface PrimaryGuild {
    identity_enabled: boolean | null;
    identity_guild_id: string | null;
    tag: string | null;
    badge: string | null;
  }

  /// `flags` is Guild Member Flags.
  interface GuildMember {
    user: User;
    nick?: string | null;
    avatar?: string | null;
    avatar_decoration_data?: AvatarDecorationData | null;
    collectibles?: Collectibles | null;
    display_name_styles?: DisplayNameStyle | null;
    banner?: string | null;
    bio?: string | null;
    roles: string[];
    joined_at: string;
    premium_since?: string | null;
    deaf?: boolean;
    mute?: boolean;
    pending?: boolean;
    communication_disabled_until?: string | null;
    unusual_dm_activity_until?: string | null;
    flags: number;
    permissions?: string;
  }

  type PartialGuildMember = Omit<GuildMember, "user">;

  interface MentionedUser extends User {
    member?: PartialGuildMember;
  }

  /// `name` is null for deleted custom emoji, though the docs type it as string.
  interface Emoji {
    id: string | null;
    name: string | null;
    roles?: string[];
    user?: User;
    require_colons?: boolean;
    managed?: boolean;
    animated?: boolean;
    available?: boolean;
    version?: string;
  }

  type PartialEmoji = Pick<Emoji, "id" | "name"> & Partial<Emoji>;

  /// `color` is deprecated in favor of `colors`.
  interface Role {
    id: string;
    name: string;
    description: string | null;
    color: number;
    colors: RoleColors;
    hoist: boolean;
    icon?: string | null;
    unicode_emoji?: string | null;
    position: number;
    permissions: string;
    managed: boolean;
    mentionable: boolean;
    flags?: number;
    tags?: RoleTags;
    version?: string;
  }

  interface RoleColors {
    primary_color: number;
    secondary_color: number | null;
    tertiary_color: number | null;
  }

  /// The `null` fields are flags, present when true and absent when false.
  interface RoleTags {
    bot_id?: string;
    integration_id?: string;
    premium_subscriber?: null;
    subscription_listing_id?: string;
    available_for_purchase?: null;
    guild_connections?: null;
  }

  /// `type` is Sticker Types, `format_type` is Sticker Format Types.
  interface Sticker {
    id: string;
    pack_id?: string;
    name: string;
    description: string | null;
    tags: string;
    type: number;
    format_type: number;
    available?: boolean;
    guild_id?: string;
    user?: User;
    sort_value?: number;
    version?: string;
  }

  interface StickerItem {
    id: string;
    name: string;
    format_type: number;
  }

  interface SoundboardSound {
    sound_id: string;
    name: string;
    volume: number;
    emoji_id: string | null;
    emoji_name: string | null;
    guild_id?: string;
    available: boolean;
    user?: User;
    user_id?: string;
  }

  /// `flags` is Attachment Flags.
  interface Attachment {
    id: string;
    filename: string;
    title?: string;
    description?: string;
    content_type?: string;
    size: number;
    url: string;
    proxy_url: string;
    height?: number | null;
    width?: number | null;
    content_scan_version?: number;
    placeholder_version?: number;
    placeholder?: string;
    ephemeral?: boolean;
    duration_secs?: number;
    waveform?: string;
    flags?: number;
    is_clip?: boolean;
    is_thumbnail?: boolean;
    is_remix?: boolean;
    is_spoiler?: boolean;
    clip_created_at?: string;
    clip_participants?: User[];
    application?: PartialApplication | null;
  }

  /// Narrowed from the partial application structure on resources/application.
  interface PartialApplication {
    id: string;
    name: string;
    description: string;
    icon: string | null;
    cover_image?: string;
    splash?: string;
    type: number | null;
    flags: number;
    [key: string]: unknown;
  }

  /// `type` is Embed Type, `flags` is Embed Flags.
  interface Embed {
    title?: string;
    type?: string;
    description?: string;
    url?: string;
    timestamp?: string;
    color?: number;
    footer?: EmbedFooter;
    image?: EmbedMedia;
    thumbnail?: EmbedMedia;
    video?: EmbedMedia;
    provider?: EmbedProvider;
    author?: EmbedAuthor;
    fields?: EmbedField[];
    reference_id?: string;
    content_scan_version?: number;
    flags?: number;
  }

  /// `flags` is Attachment Flags.
  interface EmbedMedia {
    url: string;
    proxy_url?: string;
    height?: number;
    width?: number;
    flags?: number;
    description?: string;
    content_type?: string;
    content_scan_metadata?: ContentScanMetadata;
    placeholder_version?: number;
    placeholder?: string;
  }

  interface EmbedProvider {
    name?: string;
    url?: string;
  }

  interface EmbedAuthor {
    name: string;
    url?: string;
    icon_url?: string;
    proxy_icon_url?: string;
  }

  interface EmbedFooter {
    text: string;
    icon_url?: string;
    proxy_icon_url?: string;
  }

  interface EmbedField {
    name: string;
    value: string;
    inline?: boolean;
  }

  /// `flags` is Content Scan Flags.
  interface ContentScanMetadata {
    flags: number;
    version: number;
  }

  interface Reaction {
    count: number;
    count_details: ReactionCountDetails;
    me: boolean;
    me_burst: boolean;
    emoji: PartialEmoji;
    burst_colors: string[];
  }

  interface ReactionCountDetails {
    normal: number;
    burst: number;
  }

  /// `type` is Activity Action Type on resources/presence.
  interface MessageActivity {
    type: number;
    party_id?: string;
    name_override?: string;
    icon_override?: string;
  }

  interface MessageCall {
    participants: string[];
    ended_timestamp: string | null;
  }

  /// `type` is Interaction Type, `command_type` is Application Command Type.
  interface MessageInteractionMetadata {
    id: string;
    type: number;
    name?: string;
    command_type?: number;
    ephemerality_reason?: number;
    user: User;
    authorizing_integration_owners: Record<string, string>;
    original_response_message_id?: string;
    interacted_message_id?: string;
    triggering_interaction_metadata?: MessageInteractionMetadata;
    target_user?: User;
    target_message_id?: string;
  }

  interface MessageInteraction {
    id: string;
    type: number;
    name: string;
    user: User;
  }

  interface MessageRoleSubscription {
    role_subscription_listing_id: string;
    tier_name: string;
    total_months_subscribed: number;
    is_renewal: boolean;
  }

  /// `type` is Message Purchase Notification Type.
  interface MessagePurchaseNotification {
    type: number;
    guild_product_purchase: GuildProductPurchase | null;
  }

  interface GuildProductPurchase {
    listing_id: string;
    product_name: string;
  }

  interface MessageGiftInfo {
    emoji?: PartialEmoji;
    sound?: { id: string };
  }

  /// `type` is Message Reference Type, 0 for replies and 1 for forwards.
  interface MessageReference {
    type?: number;
    message_id?: string;
    channel_id: string;
    guild_id?: string;
  }

  interface MessageSnapshot {
    message: SnapshotMessage;
  }

  interface SnapshotMessage {
    content: string;
    timestamp: string;
    edited_timestamp: string | null;
    mentions: User[];
    mention_roles: string[];
    attachments: Attachment[];
    embeds: Embed[];
    type: number;
    flags: number;
    components?: Component[];
    resolved?: ResolvedData;
    sticker_items?: StickerItem[];
    soundboard_sounds?: SoundboardSound[];
  }

  /// `layout_type` is Poll Layout Type.
  interface Poll {
    question: PollMedia;
    answers: PollAnswer[];
    expiry: string | null;
    allow_multiselect: boolean;
    layout_type: number;
    results?: PollResults;
  }

  interface PollMedia {
    text?: string;
    emoji?: PartialEmoji;
  }

  interface PollAnswer {
    answer_id: number;
    poll_media: PollMedia;
  }

  interface PollResults {
    is_finalized: boolean;
    answer_counts: PollAnswerCount[];
  }

  interface PollAnswerCount {
    id: number;
    count: number;
    me_voted: boolean;
  }

  /// `type` is Potion Type.
  interface Potion {
    used_by: string;
    type: number;
    emoji: PartialEmoji[];
    created_at: string;
  }

  /// `base_theme` is Theme on resources/user-settings-proto.
  interface SharedClientTheme {
    colors: string[];
    gradient_angle: number;
    base_mix: number;
    base_theme?: number | null;
  }

  /// `type` is Application Type on resources/application.
  interface IntegrationApplication {
    id: string;
    name: string;
    description: string;
    icon: string | null;
    cover_image?: string;
    splash?: string;
    type: number | null;
    primary_sku_id?: string;
    bot?: User;
    deeplink_uri?: string | null;
    third_party_skus?: ApplicationSku[];
    role_connections_verification_url?: string | null;
    connection_entrypoint_url?: string;
    is_verified: boolean;
    is_discoverable: boolean;
    is_monetized: boolean;
    parent_id?: string;
  }

  /// Application SKU object, resources/game.
  interface ApplicationSku {
    id: string | null;
    sku: string | null;
    distributor: string;
  }

  /// `type` is Channel Type, `flags` is Channel Flags.
  interface Channel {
    id: string;
    type: number;
    guild_id?: string;
    position?: number;
    permission_overwrites?: PermissionOverwrite[];
    name?: string | null;
    topic?: string | null;
    nsfw?: boolean;
    last_message_id?: string | null;
    last_pin_timestamp?: string | null;
    bitrate?: number;
    user_limit?: number;
    rate_limit_per_user?: number;
    recipients?: User[];
    recipient_flags?: number;
    icon?: string | null;
    nicks?: ChannelNick[];
    managed?: boolean;
    blocked_user_warning_dismissed?: boolean;
    safety_warnings?: SafetyWarning[];
    application_id?: string;
    owner_id?: string;
    owner?: GuildMember | null;
    parent_id?: string | null;
    rtc_region?: string | null;
    video_quality_mode?: number;
    total_message_sent?: number;
    message_count?: number;
    member_count?: number;
    member_ids_preview?: string[];
    thread_metadata?: ThreadMetadata;
    member?: ThreadMember;
    default_auto_archive_duration?: number | null;
    default_thread_rate_limit_per_user?: number;
    permissions?: string;
    flags?: number;
    available_tags?: ForumTag[];
    applied_tags?: string[];
    default_reaction_emoji?: DefaultReaction | null;
    default_forum_layout?: number;
    default_sort_order?: number | null;
    default_tag_setting?: string;
    icon_emoji?: IconEmoji | null;
    theme_color?: number | null;
    is_message_request?: boolean;
    is_message_request_timestamp?: string | null;
    is_spam?: boolean;
    status?: string | null;
    hd_streaming_until?: string | null;
    hd_streaming_buyer_id?: string | null;
    linked_lobby?: LinkedLobby | null;
    is_linkable?: boolean;
    is_viewable_and_writeable_by_all_members?: boolean;
    template?: string;
    version?: string;
  }

  interface PartialChannel {
    id: string;
    type: number;
    name: string | null;
    recipients?: User[];
    icon?: string | null;
    guild_id?: string | null;
  }

  /// `type` is Permission Overwrite Type, 0 for role and 1 for member.
  interface PermissionOverwrite {
    id: string;
    type: number;
    allow: string;
    deny: string;
  }

  interface ChannelNick {
    id: string;
    nick: string;
  }

  /// `type` is Safety Warning Type.
  interface SafetyWarning {
    id: string;
    type: number;
    expiry: string;
    dismiss_timestamp: string | null;
  }

  interface ThreadMetadata {
    archived: boolean;
    auto_archive_duration: number;
    archive_timestamp: string;
    locked: boolean;
    invitable?: boolean;
    create_timestamp?: string | null;
  }

  /// `flags` is Thread Member Flags.
  interface ThreadMember {
    id?: string;
    user_id?: string;
    join_timestamp: string;
    flags: number;
    muted?: boolean;
    mute_config?: MuteConfig | null;
    member?: GuildMember;
  }

  interface MuteConfig {
    end_time?: string | null;
    selected_time_window?: number;
  }

  interface ForumTag {
    id: string;
    name: string;
    moderated: boolean;
    emoji_id: string | null;
    emoji_name: string | null;
  }

  interface DefaultReaction {
    emoji_id: string | null;
    emoji_name: string | null;
  }

  interface IconEmoji {
    id: string | null;
    name: string | null;
  }

  interface LinkedLobby {
    application_id: string;
    lobby_id: string;
    linked_by: string;
    linked_at: string;
    require_application_authorization: boolean;
  }

  /// Resolved data object, interactions/receiving-and-responding. Maps are keyed by snowflake.
  interface ResolvedData {
    users?: Record<string, User>;
    members?: Record<string, Omit<GuildMember, "user" | "deaf" | "mute">>;
    roles?: Record<string, Role>;
    channels?: Record<
      string,
      Omit<Channel, "permission_overwrites" | "recipients" | "icon" | "application_id" | "managed" | "member">
    >;
    messages?: Record<string, Message>;
    attachments?: Record<string, Attachment>;
  }

  /// Message components from resources/components. Modal-only types are left out.
  type Component =
    | ActionRow
    | Button
    | StringSelect
    | UserSelect
    | RoleSelect
    | MentionableSelect
    | ChannelSelect
    | Section
    | TextDisplay
    | Thumbnail
    | MediaGallery
    | FileComponent
    | Separator
    | ContentInventoryEntryComponent
    | Container
    | CheckpointCard;

  interface ActionRow {
    type: 1;
    id?: number;
    components: Component[];
  }

  /// `style` is Button Style. The docs mark `custom_id` required, but link and premium buttons omit it.
  interface Button {
    type: 2;
    id?: number;
    style: number;
    label?: string;
    emoji?: PartialEmoji;
    custom_id?: string;
    sku_id?: string;
    url?: string;
    disabled?: boolean;
  }

  interface SelectBase {
    id?: number;
    custom_id: string;
    placeholder?: string;
    min_values?: number;
    max_values?: number;
    required?: boolean;
    disabled?: boolean;
  }

  interface StringSelect extends SelectBase {
    type: 3;
    options: SelectOption[];
  }

  interface SelectOption {
    label: string;
    value: string;
    description?: string;
    emoji?: PartialEmoji;
    default?: boolean;
  }

  interface UserSelect extends SelectBase {
    type: 5;
    default_values?: SelectDefaultValue[];
  }

  interface RoleSelect extends SelectBase {
    type: 6;
    default_values?: SelectDefaultValue[];
  }

  interface MentionableSelect extends SelectBase {
    type: 7;
    default_values?: SelectDefaultValue[];
  }

  /// `channel_types` is Channel Type.
  interface ChannelSelect extends SelectBase {
    type: 8;
    channel_types?: number[];
    default_values?: SelectDefaultValue[];
  }

  interface SelectDefaultValue {
    id: string;
    type: string;
  }

  interface Section {
    type: 9;
    id?: number;
    components: TextDisplay[];
    accessory: Thumbnail | Button;
  }

  interface TextDisplay {
    type: 10;
    id?: number;
    content: string;
  }

  interface Thumbnail {
    type: 11;
    id?: number;
    media: UnfurledMediaItem;
    description?: string | null;
    spoiler?: boolean;
  }

  interface MediaGallery {
    type: 12;
    id?: number;
    items: MediaGalleryItem[];
  }

  interface MediaGalleryItem {
    media: UnfurledMediaItem;
    description?: string | null;
    spoiler?: boolean;
  }

  interface FileComponent {
    type: 13;
    id?: number;
    file: UnfurledMediaItem;
    spoiler?: boolean;
    name: string;
    size: number;
  }

  /// `spacing` is Separator Spacing Type.
  interface Separator {
    type: 14;
    id?: number;
    divider?: boolean;
    spacing?: number;
  }

  /// The entry is left untyped, see Content Inventory Entry Object on resources/components.
  interface ContentInventoryEntryComponent {
    type: 16;
    id?: number;
    content_inventory_entry: Record<string, unknown>;
  }

  interface Container {
    type: 17;
    id?: number;
    components: Component[];
    accent_color?: number | null;
    spoiler?: boolean;
  }

  /// The data is left untyped, see Checkpoint Data Structure on resources/components.
  interface CheckpointCard {
    type: 20;
    id?: number;
    checkpoint_data: Record<string, unknown>;
  }

  /// `flags` is Attachment Flags, `loading_state` is Unfurled Media Item Loading State.
  interface UnfurledMediaItem {
    id?: string;
    url: string;
    proxy_url?: string;
    height?: number | null;
    width?: number | null;
    flags?: number;
    content_type?: string;
    content_scan_metadata?: ContentScanMetadata;
    placeholder_version?: number;
    placeholder?: string;
    loading_state?: number;
    attachment_id?: string;
  }

  /// Message object, resources/message. `type` is Message Type, `flags` is Message Flags.
  interface Message {
    id: string;
    channel_id: string;
    lobby_id?: string;
    author: User;
    content: string;
    timestamp: string;
    edited_timestamp: string | null;
    tts: boolean;
    mention_everyone: boolean;
    mentions: User[];
    mention_roles: string[];
    mention_channels?: PartialChannel[];
    attachments: Attachment[];
    embeds: Embed[];
    reactions?: Reaction[];
    nonce?: number | string;
    pinned: boolean;
    webhook_id?: string;
    type: number;
    activity?: MessageActivity;
    application?: IntegrationApplication;
    application_id?: string;
    flags: number;
    message_reference?: MessageReference;
    referenced_message?: Message | null;
    message_snapshots?: MessageSnapshot[];
    call?: MessageCall;
    interaction?: MessageInteraction;
    interaction_metadata?: MessageInteractionMetadata;
    resolved?: ResolvedData;
    thread?: Channel;
    role_subscription_data?: MessageRoleSubscription;
    purchase_notification?: MessagePurchaseNotification;
    gift_info?: MessageGiftInfo;
    components: Component[];
    sticker_items?: StickerItem[];
    stickers?: Sticker[];
    poll?: Poll;
    changelog_id?: string;
    soundboard_sounds?: SoundboardSound[];
    potions?: Potion[];
    shared_client_theme?: SharedClientTheme;
  }

  /// Message object extra fields, gateway/gateway-events. `channel_type` is Channel Type.
  interface GatewayMessage extends Message {
    channel_type: number;
    guild_id?: string;
    member?: PartialGuildMember;
    mentions: MentionedUser[];
    metadata?: Record<string, string>;
    moderation_metadata?: Record<string, string>;
  }

  interface MessageDelete {
    id: string;
    channel_id: string;
    guild_id?: string;
  }

  interface MessageDeleteBulk {
    ids: string[];
    channel_id: string;
    guild_id?: string;
  }

  const sig: unique symbol;

  /// An opaque hook point handed out by the runtime. `P` is the payload its hooks receive.
  interface Sig<P> {
    readonly [sig]: P;
  }

  /// Payload types for gateway events, keyed like `plumage.gateway`.
  interface GatewayPayloads {
    messageCreate: GatewayMessage;
    /// A full message per the docs, with `tts` always false.
    messageUpdate: GatewayMessage;
    messageDelete: MessageDelete;
    messageDeleteBulk: MessageDeleteBulk;
  }

  /// One handle per entry in the client's gateway registry, read from the
  /// running binary. Events without a payload type here are `Sig<unknown>`, and
  /// any event can be missing from a given build.
  type Gateway = { readonly [E in keyof GatewayPayloads]: Sig<GatewayPayloads[E]> } & {
    readonly [event: string]: Sig<unknown> | undefined;
  };

  interface Hooks<P> {
    /// Runs before the client's handler. Return a payload to replace it, `false` to drop the
    /// event, or nothing to pass the (possibly mutated) payload on. The replacement is
    /// re-serialized with JSON.stringify, so integers above 2^53 lose precision.
    before?(payload: P): P | false | void;
    /// Runs after the client's handler with the payload it saw. `ok` is false when the handler
    /// returned an error.
    after?(payload: P, ok: boolean): void;
  }

  interface Log {
    debug(...args: unknown[]): void;
    info(...args: unknown[]): void;
    warn(...args: unknown[]): void;
    error(...args: unknown[]): void;
  }

  /// Per-plugin JSON storage, written to disk on every change.
  interface Storage {
    get<T = unknown>(key: string): T | undefined;
    set(key: string, value: unknown): void;
    delete(key: string): void;
    keys(): string[];
  }

  interface SpecBase {
    title: string;
    description?: string;
  }

  type SettingSpec =
    | (SpecBase & { type: "switch"; default?: boolean })
    | (SpecBase & { type: "text"; default?: string })
    | (SpecBase & { type: "number"; default?: number })
    | (SpecBase & { type: "button"; onPress(): void });

  /// The value a setting reads as. Buttons have none.
  type SettingValue<S> = S extends { type: "switch" }
    ? boolean
    : S extends { type: "text" }
      ? string
      : S extends { type: "number" }
        ? number
        : never;

  type SettingValues<S> = {
    readonly [K in keyof S as S[K] extends { type: "button" } ? never : K]: SettingValue<S[K]>;
  };

  interface Settings {
    /// Shows the settings on the plugin's page in Settings. Each property of the result reads
    /// the user's current choice, so read it when needed instead of caching it. Text and number
    /// fields save when the keyboard's go key is pressed.
    define<const S extends Record<string, SettingSpec>>(schema: S): SettingValues<S>;
  }

  interface Api {
    readonly gateway: Gateway;
    /// Observes a hook point after the client handled it.
    on<P>(sig: Sig<P>, listener: (payload: P) => void): void;
    hook<P>(sig: Sig<P>, hooks: Hooks<P>): void;
    /// Feeds `payload` to the client's own handler for `sig`, skipping every plugin. Works only
    /// while a gateway hook runs, and throws elsewhere. Returns whether the handler accepted it.
    dispatch<P>(sig: Sig<P>, payload: Partial<P>): boolean;
    readonly log: Log;
    readonly storage: Storage;
    readonly settings: Settings;
  }
}

declare const plumage: Plumage.Api;
