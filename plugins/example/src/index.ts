// Example plugin: logs incoming messages and rewrites ":shrug:" before Phoenix stores them.
// Build: npm i && npm run build -> dist/example.js (drop it into files/phoenix-js or the patcher's plugin list).

interface Message {
  id: string;
  channel_id: string;
  content: string;
  author?: { username?: string };
}

const SHRUG = '¯\\_(ツ)_/¯';

phoenix.on<Message>('MESSAGE_CREATE', (ev) => {
  const m = ev.data;
  console.log(`${m.author?.username ?? '?'} in ${m.channel_id}: ${m.content}`);
  if (!m.content.includes(':shrug:')) return; // pass through untouched
  return { ...m, content: m.content.split(':shrug:').join(SHRUG) };
});

const runs = phoenix.storage.get<number>('runs', 0) + 1;
phoenix.storage.set('runs', runs);
console.log(`example plugin loaded (run ${runs})`);
