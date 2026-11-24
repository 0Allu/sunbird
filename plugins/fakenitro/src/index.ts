// @description Sends custom emojis from any server, and animated ones, as image links
// @version 0.2.1
// @author mantikafasi

const { gateway } = phoenix;

const settings = phoenix.settings.define({
  emojiSize: {
    type: "select",
    title: "Emoji size",
    options: [
      { value: "48" },
      { value: "32" },
      { value: "64" },
      { value: "96" },
      { value: "128" },
      { value: "160" },
    ],
  },
  useHyperLinks: {
    type: "switch",
    title: "Hide links behind the name",
    description: "Send [name](link) instead of the bare link",
    default: true,
  },
  hyperLinkText: {
    type: "text",
    title: "Link text",
    description: "{{NAME}} is replaced with the emoji's name",
    default: "{{NAME}}",
  },
});

interface Emoji {
  id: string;
  name: string;
  animated: boolean;
  available: boolean;
  guild: string;
}

const emojis = new Map<string, Emoji>();
const byName = new Map<string, Emoji[]>();
const channelGuild = new Map<string, string>();
// The emoji last picked for each name, which `:name:` in the message box means.
const picked = new Map<string, Emoji>();
// The real tier, before any spoofing. Unknown until the app's first API request.
let premiumType = 0;

const addEmojis = (guild: string, list: Phoenix.Emoji[] | undefined) => {
  for (const e of list ?? []) {
    if (!e.id || !e.name || emojis.has(e.id)) continue;
    const emoji = { id: e.id, name: e.name, animated: !!e.animated, available: e.available !== false, guild };
    emojis.set(e.id, emoji);
    byName.set(e.name, [...(byName.get(e.name) ?? []), emoji]);
  }
};

// Phoenix loads guilds lazily and never hands READY to plugins: guilds and their
// emojis live in its cache. Ids in keys are zero-padded to 20 digits.
const unpad = (id: string) => id.replace(/^0+/, "");
const load = async () => {
  const guilds = await phoenix.stores.sql<{ g: string; e: string | null }>(
    "SELECT k AS g, json_extract(CAST(v AS TEXT), '$.emojis') AS e FROM guilds",
  );
  for (const { g, e } of guilds) if (e) addEmojis(unpad(g), JSON.parse(e));
  // One row per guild keeps this under the 10000-row limit.
  const channels = await phoenix.stores.sql<{ g: string; c: string }>(
    "SELECT substr(k, 1, 20) AS g, group_concat(substr(k, 22)) AS c FROM channel_stubs" +
      " WHERE k NOT LIKE '00000000000000000000:%' GROUP BY 1",
  );
  for (const { g, c } of channels) for (const id of c.split(",")) channelGuild.set(unpad(id), unpad(g));
  phoenix.log.info(`${emojis.size} emojis in ${guilds.length} servers, ${channelGuild.size} channels`);
};
load().catch((err) => phoenix.log.warn(`reading the cache: ${err}`));

if (gateway.guildCreate) {
  phoenix.on(gateway.guildCreate, (guild) => {
    addEmojis(guild.id, guild.emojis);
    for (const channel of guild.channels ?? []) channelGuild.set(channel.id, guild.id);
  });
}
if (gateway.guildEmojisUpdate) {
  phoenix.on(gateway.guildEmojisUpdate, (update) => addEmojis(update.guild_id, update.emojis));
}

const readTier = () =>
  phoenix.discord
    .me()
    .then((user) => (premiumType = user?.premium_type ?? 0))
    .catch((err) => phoenix.log.warn(`reading the account: ${err}`));
readTier();
if (gateway.readySupplemental) phoenix.on(gateway.readySupplemental, readTier);

// What Discord lets a non-Nitro user send: the server's own static emojis.
// With Nitro, anything (external emoji permission is not checked).
const usable = (emoji: Emoji, guild: string | undefined) =>
  emoji.available && (premiumType > 0 || (!emoji.animated && emoji.guild === guild));

const link = (emoji: Emoji) => {
  const size = settings.emojiSize;
  const name = encodeURIComponent(emoji.name);
  const url = `https://cdn.discordapp.com/emojis/${emoji.id}.${emoji.animated ? "gif" : "webp"}?size=${size}&name=${name}&lossless=true`;
  if (!settings.useHyperLinks) return url;
  return `[${settings.hyperLinkText.replaceAll("{{NAME}}", emoji.name)}](${url})`;
};

// Links need whitespace around them to embed.
const spaced = (text: string, offset: number, length: number, replacement: string) => {
  const before = offset > 0 && !/\s/.test(text[offset - 1]) ? " " : "";
  const after = offset + length < text.length && !/\s/.test(text[offset + length]) ? " " : "";
  return before + replacement + after;
};

// Rewrites `<a:name:id>` tokens and `:name:` shortcodes the sender can't use.
// ponytail: a `:name:` shared by several servers picks the first one we saw
// when none of them is usable here.
const rewrite = (text: string, guild: string | undefined) =>
  text.replace(/(?<!\\)<a?:(\w+):(\d+)>|(?<![\\<]):(\w+):(?!\d)/g, (match, _n, id, name, offset, whole) => {
    let emoji: Emoji | undefined;
    if (id) {
      emoji = emojis.get(id);
    } else if (picked.has(name)) {
      emoji = picked.get(name);
    } else {
      const found = byName.get(name);
      if (!found || found.some((e) => usable(e, guild))) return match;
      emoji = found.find((e) => e.available);
    }
    if (!emoji || usable(emoji, guild)) return match;
    return spaced(whole, offset, match.length, link(emoji));
  });

const guildOf = (ctx: Phoenix.UiContext) => (ctx.channelId ? channelGuild.get(ctx.channelId) : undefined);

// Phoenix answers a press on a locked emoji with a Nitro ad. Put `:name:` in
// the message box instead, like an unlocked pick, and remember which emoji it
// was, since several servers can have one by that name.
phoenix.messages.onEmojiPick((id, ctx) => {
  const emoji = emojis.get(id);
  if (!emoji || usable(emoji, guildOf(ctx))) return;
  picked.set(emoji.name, emoji);
  return `:${emoji.name}: `;
});

phoenix.messages.onSend((text, ctx) => {
  if (!text.includes(":")) return;
  const result = rewrite(text, guildOf(ctx));
  return result === text ? undefined : result;
});
