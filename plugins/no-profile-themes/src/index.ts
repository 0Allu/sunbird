// @description Strips profile themes, effects, avatar decorations and display name styles
// @version 0.1.0
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

const ready = gateway.ready as Phoenix.Sig<{ user?: { id: string } }> | undefined;
if (ready) {
  phoenix.hook(ready, {
    before(payload) {
      self = payload.user?.id;
      strip(payload);
    },
  });
}

const events = [
  "readySupplemental",
  "initialGuild",
  "guildCreate",
  "messageCreate",
  "messageUpdate",
  "guildMemberAdd",
  "guildMemberUpdate",
  "guildMembersChunk",
  "guildMemberListUpdate",
  "presenceUpdate",
  "presencesReplace",
  "threadMembersUpdate",
  "channelCreate",
  "relationshipAdd",
  "relationshipUpdate",
];
for (const event of events) {
  const sig = gateway[event];
  if (sig) phoenix.hook(sig, { before: strip });
}
