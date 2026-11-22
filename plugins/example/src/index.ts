// @name Example
// @description Rewrites :shrug: and answers "!ping" with a local "pong" only you see.
// @version 1.0.0
// @author phoenix-unlock
// @api 1
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

phoenix.exports.shrug = SHRUG; // other plugins: phoenix.plugins.get('example')?.shrug
