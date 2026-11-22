// @name Example
// @description Rewrites :shrug:, answers "!ping" with a local "pong", adds /shrug and a "Message info" action.
// @version 1.0.0
// @author phoenix-unlock
// @api 2
//
// Build: npm i && npm run build -> dist/example.js, then Settings → Phoenix → Install plugin.

phoenix.settings.define({
  shrug: { type: 'switch', title: 'Rewrite :shrug:', description: 'Turns :shrug: into ¯\\_(ツ)_/¯ in messages you receive.', default: true },
  trigger: { type: 'text', title: 'Ping command', description: 'What to answer with a local pong', default: '!ping' },
});

const SHRUG = '¯\\_(ツ)_/¯';
let myId: string | undefined;

phoenix.ready.then(async () => {
  myId = (await phoenix.stores.me())?.id;
  phoenix.ui.toast('Example plugin is running');
});

phoenix.on('MESSAGE_CREATE', (ev) => {
  const m = ev.data;
  if (m.author.id === myId && m.content.trim() === phoenix.settings.get<string>('trigger')) {
    phoenix.messages.addLocal(m.channel_id, `pong (${phoenix.version.arch}, plugin API ${phoenix.version.api})`, { guildId: m.guild_id });
    return;
  }
  if (!phoenix.settings.get('shrug') || !m.content.includes(':shrug:')) return; // pass through untouched
  return { ...m, content: m.content.split(':shrug:').join(SHRUG) };
});

// UI patch targets: a slash command and a row in the message long-press menu
phoenix.commands.register({ name: 'shrug', description: 'Sends your text with a shrug', execute: (args) => `${args} ${SHRUG}`.trim() });
phoenix.ui.addMessageAction({
  label: 'Message info', icon: 'res://ic_mana_id',
  onPress: (ctx) => ctx.reply(`id ${ctx.messageId}, by ${ctx.message?.author?.username ?? '?'}, channel ${ctx.channelId}`),
});

phoenix.exports.shrug = SHRUG; // other plugins: phoenix.plugins.get('example')?.shrug
