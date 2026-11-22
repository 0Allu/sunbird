const { gateway } = plumage;

plumage.on(gateway.messageCreate, (message) => {
  plumage.log.info(`#${message.channel_id} <${message.author.username}> ${message.content}`);
});

plumage.hook(gateway.messageCreate, {
  before(message) {
    const seen = (plumage.storage.get<number>("seen") ?? 0) + 1;
    plumage.storage.set("seen", seen);
    message.content = `${message.content.toUpperCase()} [plumage #${seen}]`;
  },
});
