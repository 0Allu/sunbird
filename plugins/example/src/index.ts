// @description Rewrites incoming messages to show off plugin settings
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
