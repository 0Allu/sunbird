// @description Strips profile themes, effects, avatar decorations, and name styles
// @version 0.1.1
// @author amaanq

const { gateway } = phoenix;

const settings = phoenix.settings.define({
  keepOwn: {
    type: "switch",
    title: "Keep my own",
    description: "Leave your own profile theming in place",
    default: true,
  },
});

const THEMING = [
  "avatar_decoration_data",
  "collectibles",
  "display_name_styles",
  "theme_colors",
  "profile_effect",
];

let self: string | undefined;

type Node = Record<string, unknown>;

const strip = (value: unknown) => {
  if (Array.isArray(value)) {
    for (const item of value) strip(item);
    return;
  }
  if (typeof value !== "object" || value === null) return;
  const node = value as Node;
  const owner = (node.user as Node | undefined)?.id ?? node.id;
  const keep = settings.keepOwn && self !== undefined && owner === self;
  for (const key of Object.keys(node)) {
    if (!keep && THEMING.includes(key)) delete node[key];
    else strip(node[key]);
  }
};

const readSelf = () =>
  phoenix.discord
    .me()
    .then((user) => (self = user?.id))
    .catch((err) => phoenix.log.warn(`reading the account: ${err}`));
readSelf();
if (gateway.readySupplemental) phoenix.on(gateway.readySupplemental, readSelf);

const events = [
  gateway.readySupplemental,
  gateway.initialGuild,
  gateway.guildCreate,
  gateway.messageCreate,
  gateway.messageUpdate,
  gateway.guildMemberAdd,
  gateway.guildMemberUpdate,
  gateway.guildMembersChunk,
  gateway.guildMemberListUpdate,
  gateway.presenceUpdate,
  gateway.presencesReplace,
  gateway.threadMembersUpdate,
  gateway.channelCreate,
  gateway.relationshipAdd,
  gateway.relationshipUpdate,
];
for (const sig of events) {
  if (sig) phoenix.hook(sig, { before: strip });
}
