// MessageLogger: deleted messages stay (shown in red, marked "(deleted)"); edited messages keep
// their earlier versions under the new text, newest first.
const MAX = 8192;
const cache = new Map(); // id -> { channel_id, guild_id, content, hist: string[], deleted }

const remember = (m) => {
  let e = cache.get(m.id);
  if (!e) {
    e = { hist: [] };
    cache.set(m.id, e);
    if (cache.size > MAX) cache.delete(cache.keys().next().value); // oldest first
  }
  e.channel_id = m.channel_id;
  if (m.guild_id) e.guild_id = m.guild_id;
  e.content = m.content;
  return e;
};
const subtext = (s, suffix) => s.split('\n').map((l) => '-# ' + l).join('\n') + suffix;

phoenix.on('MESSAGE_CREATE', (ev) => {
  const m = ev.data;
  if (m.id && typeof m.content === 'string') remember(m);
});

phoenix.on('MESSAGE_UPDATE', (ev) => {
  const m = ev.data;
  if (!m.id || !m.channel_id || typeof m.content !== 'string') return; // e.g. embed-only updates
  const old = cache.get(m.id);
  if (old && typeof old.content === 'string' && old.content !== m.content) old.hist.unshift(subtext(old.content, ' (edited)'));
  const e = remember(m);
  if (!e.hist.length) return;
  return { ...m, content: (m.content ? m.content + '\n' : '') + e.hist.join('\n') };
});

const markDeleted = (id) => {
  const e = cache.get(id);
  if (!e || e.deleted || typeof e.content !== 'string') return;
  e.deleted = true;
  let content = e.content ? '```diff\n' + e.content.split('\n').map((l) => '- ' + l).join('\n') + '\n```\n' : '';
  if (e.hist.length) content += e.hist.join('\n') + '\n';
  content += '-# (deleted)';
  // straight to Phoenix's own MESSAGE_UPDATE handler (our listener above does not see it)
  phoenix.dispatch('MESSAGE_UPDATE', { id, channel_id: e.channel_id, ...(e.guild_id ? { guild_id: e.guild_id } : {}), content });
};

// Returning false drops the event: Phoenix never removes the message.
phoenix.on('MESSAGE_DELETE', (ev) => { markDeleted(ev.data.id); return false; });
phoenix.on('MESSAGE_DELETE_BULK', (ev) => { for (const id of ev.data.ids || []) markDeleted(id); return false; });
