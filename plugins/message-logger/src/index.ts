const { gateway } = plumage;

const settings = plumage.settings.define({
  keepDeleted: { title: "Keep deleted messages", description: "Show deleted messages struck out", default: true },
  keepEdits: { title: "Keep edit history", description: "Show earlier versions under edited messages", default: true },
});

interface Seen {
  channelId: string;
  guildId?: string;
  content: string;
  history: string[];
  deleted: boolean;
}

const limit = 8192;
const seen = new Map<string, Seen>();

const remember = (message: { id: string; channel_id: string; guild_id?: string; content: string }) => {
  let entry = seen.get(message.id);
  if (!entry) {
    entry = { channelId: message.channel_id, content: message.content, history: [], deleted: false };
    seen.set(message.id, entry);
    if (seen.size > limit) seen.delete(seen.keys().next().value!);
  }
  entry.channelId = message.channel_id;
  entry.guildId = message.guild_id ?? entry.guildId;
  entry.content = message.content;
  return entry;
};

const subtext = (text: string, suffix: string) =>
  text
    .split("\n")
    .map((line) => `-# ${line}`)
    .join("\n") + suffix;

plumage.on(gateway.messageCreate, (message) => {
  if (message.id && typeof message.content === "string") remember(message);
});

plumage.hook(gateway.messageUpdate, {
  before(message) {
    // Embed-only updates carry no content.
    if (!message.id || !message.channel_id || typeof message.content !== "string") return;
    const old = seen.get(message.id);
    if (old && old.content !== message.content) old.history.unshift(subtext(old.content, " (edited)"));
    const entry = remember(message);
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
  content += "-# (deleted)";
  plumage.dispatch(gateway.messageUpdate, {
    id,
    channel_id: entry.channelId,
    ...(entry.guildId ? { guild_id: entry.guildId } : {}),
    content,
  });
  return true;
};

plumage.hook(gateway.messageDelete, {
  before(deleted) {
    if (settings.keepDeleted && markDeleted(deleted.id)) return false;
  },
});

plumage.hook(gateway.messageDeleteBulk, {
  before(deleted) {
    if (!settings.keepDeleted) return;
    deleted.ids = deleted.ids.filter((id) => !markDeleted(id));
    if (!deleted.ids.length) return false;
  },
});
