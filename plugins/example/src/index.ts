// @description Shows off plugin settings, UI patch targets and slash commands
// @version 0.1.0
// @author amaanq

const { gateway } = plumage;

const settings = plumage.settings.define({
  shout: { type: "switch", title: "Shout", description: "Upper-case incoming messages", default: true },
  suffix: { type: "text", title: "Suffix", description: "Appended to each message", default: "[plumage]" },
  every: { type: "number", title: "Every nth message", description: "Change 1 in this many", default: 1 },
  reset: {
    type: "button",
    title: "Reset counter",
    onPress() {
      plumage.storage.set("seen", 0);
      plumage.log.info("counter reset");
    },
  },
});

plumage.on(gateway.messageCreate, (message) => {
  plumage.log.info(`#${message.channel_id} <${message.author.username}> ${message.content}`);
});

plumage.hook(gateway.messageCreate, {
  before(message) {
    const seen = (plumage.storage.get<number>("seen") ?? 0) + 1;
    plumage.storage.set("seen", seen);
    if (seen % Math.max(1, Math.round(settings.every)) !== 0) return;
    const content = settings.shout ? message.content.toUpperCase() : message.content;
    message.content = `${content} ${settings.suffix} #${seen}`;
  },
});

plumage.ui.addHeaderButton({
  icon: "res://ic_mana_puzzle_piece_plus",
  label: "Example",
  onPress: (ctx) => ctx.reply(`header button pressed in ${ctx.channelId}`),
});

plumage.ui.addComposerButton({
  icon: "res://ic_mana_angle_brackets",
  label: "Shrug",
  onPress: (ctx) => ctx.reply("¯\\_(ツ)_/¯"),
});

plumage.ui.addMessageAccessory({
  placement: "header",
  render: ({ messageId }) => `#${messageId.slice(-4)}`,
});

plumage.ui.addMessageAction({
  label: "Copy id to chat",
  icon: "res://ic_mana_id",
  onPress: (ctx) => ctx.reply(`message ${ctx.messageId} by ${ctx.message?.author.username ?? "someone"}`),
});

plumage.ui.addProfileSection({
  title: "Plumage",
  render: ({ userId }) => (userId ? `user ${userId}` : null),
});

plumage.commands.register({
  name: "echo",
  description: "Sends its arguments back to you only",
  execute: (args, ctx) => {
    ctx.reply(args || "(nothing)");
  },
});

plumage.messages.onSend((text) => text.replace(/\bteh\b/g, "the"));
