// @description Keeps deleted messages and edit history
// @version 0.1.0
// @author amaanq

const { gateway } = phoenix;

const settings = phoenix.settings.define({
  keepDeleted: {
    type: "switch",
    title: "Keep deleted messages",
    description: "Show deleted messages struck out",
    default: true,
  },
  keepEdits: {
    type: "switch",
    title: "Keep edit history",
    description: "Show earlier versions under edited messages",
    default: true,
  },
  keepAttachments: {
    type: "switch",
    title: "Keep removed attachments",
    description: "Leave attachments an edit took off the message in place",
    default: true,
  },
  ignoreBots: {
    type: "switch",
    title: "Ignore bots",
    description: "Skip messages sent by bots",
    default: true,
  },
  ignoreUsers: {
    type: "text",
    title: "Ignored users",
    description: "User IDs separated by commas or spaces",
    default: "",
  },
  ignoreChannels: {
    type: "text",
    title: "Ignored channels",
    description: "Channel IDs separated by commas or spaces",
    default: "",
  },
  ignoreGuilds: {
    type: "text",
    title: "Ignored servers",
    description: "Server IDs separated by commas or spaces",
    default: "",
  },
});

interface Seen {
  channelId: string;
  guildId?: string;
  content: string;
  attachments: Phoenix.Attachment[];
  history: string[];
  deleted: boolean;
}

const EPHEMERAL = 64;
const limit = 8192;
const seen = new Map<string, Seen>();

const ids = (list: string) => list.split(/[\s,]+/).filter(Boolean);

const ignored = (message: Phoenix.GatewayMessage) =>
  (message.flags & EPHEMERAL) !== 0 ||
  (settings.ignoreBots && message.author?.bot === true) ||
  ids(settings.ignoreUsers).includes(message.author?.id) ||
  ids(settings.ignoreChannels).includes(message.channel_id) ||
  (message.guild_id !== undefined && ids(settings.ignoreGuilds).includes(message.guild_id));

const remember = (message: Phoenix.GatewayMessage) => {
  let entry = seen.get(message.id);
  if (!entry) {
    entry = {
      channelId: message.channel_id,
      content: message.content,
      attachments: [],
      history: [],
      deleted: false,
    };
    seen.set(message.id, entry);
    if (seen.size > limit) seen.delete(seen.keys().next().value!);
  }
  entry.channelId = message.channel_id;
  entry.guildId = message.guild_id ?? entry.guildId;
  entry.content = message.content;
  entry.attachments = message.attachments ?? entry.attachments;
  return entry;
};

const unix = (timestamp: string | null | undefined) =>
  Math.floor((timestamp ? Date.parse(timestamp) : Date.now()) / 1000);

const subtext = (text: string, suffix: string) =>
  text
    .split("\n")
    .map((line) => `-# ${line}`)
    .join("\n") + suffix;

phoenix.hook(gateway.messageCreate, {
  before(message) {
    if (!message.id || typeof message.content !== "string" || ignored(message)) return;
    // A create whose nonce names an existing message replaces that message in
    // place, which would erase it without a delete event.
    const nonce = message.nonce === undefined ? undefined : String(message.nonce);
    if (nonce && nonce !== message.id && seen.has(nonce)) delete message.nonce;
    remember(message);
  },
});

phoenix.hook(gateway.messageUpdate, {
  before(message) {
    // Embed-only updates carry no content.
    if (!message.id || !message.channel_id || typeof message.content !== "string") return;
    if (ignored(message)) return;
    const old = seen.get(message.id);
    const when = unix(message.edited_timestamp);
    if (old && old.content !== message.content) {
      old.history.unshift(subtext(old.content, ` (edited <t:${when}:R>)`));
    }
    const removed = old?.attachments.filter(
      (attachment) => !message.attachments?.some((kept) => kept.id === attachment.id),
    ) ?? [];
    const entry = remember(message);
    if (removed.length && settings.keepAttachments) {
      message.attachments = [...(message.attachments ?? []), ...removed];
      entry.attachments = message.attachments;
      const names = removed.map((attachment) => attachment.filename).join(", ");
      entry.history.unshift(`-# removed ${names} <t:${when}:R>`);
    }
    if (!entry.history.length || !settings.keepEdits) return;
    message.content = (message.content ? `${message.content}\n` : "") + entry.history.join("\n");
  },
});

// Messages from before the app started have no saved text, so their deletes
// pass through.
const markDeleted = (id: string) => {
  const entry = seen.get(id);
  if (!entry) return false;
  if (entry.deleted) return true;
  entry.deleted = true;
  const lines = entry.content.split("\n").map((line) => `- ${line}`);
  let content = entry.content ? `\`\`\`diff\n${lines.join("\n")}\n\`\`\`\n` : "";
  if (entry.history.length) content += `${entry.history.join("\n")}\n`;
  content += `-# (deleted <t:${unix(undefined)}:R>)`;
  phoenix.dispatch(gateway.messageUpdate, {
    id,
    channel_id: entry.channelId,
    ...(entry.guildId ? { guild_id: entry.guildId } : {}),
    content,
  });
  return true;
};

phoenix.hook(gateway.messageDelete, {
  before(deleted) {
    if (settings.keepDeleted && markDeleted(deleted.id)) return false;
  },
});

phoenix.hook(gateway.messageDeleteBulk, {
  before(deleted) {
    if (!settings.keepDeleted) return;
    deleted.ids = deleted.ids.filter((id) => !markDeleted(id));
    if (!deleted.ids.length) return false;
  },
});
