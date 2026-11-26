// @description Shows which devices people are online from: desktop, mobile, web or console
// @version 1.0.0
// @author yuax
// @warning On Android and iOS, profile badges load their icons from cdn.jsdelivr.net (Twemoji)

type Status = "online" | "idle" | "dnd";
type Platform = "desktop" | "mobile" | "web" | "embedded" | "vr";
type Presence = { status: Status; platforms: Partial<Record<Platform, Status>> };

const PLATFORMS: Record<Platform, { label: string; emoji: string; twemoji: string; icon: string }> = {
  desktop: {
    label: "Desktop",
    emoji: "🖥️",
    twemoji: "1f5a5",
    icon: '<path fill-rule="evenodd" d="M3 4h18a1 1 0 0 1 1 1v11a1 1 0 0 1-1 1h-7v2h3v2H7v-2h3v-2H3a1 1 0 0 1-1-1V5a1 1 0 0 1 1-1Zm1 2v9h16V6H4Z"/>',
  },
  mobile: {
    label: "Mobile",
    emoji: "📱",
    twemoji: "1f4f1",
    icon: '<path fill-rule="evenodd" d="M8 2h8a2 2 0 0 1 2 2v16a2 2 0 0 1-2 2H8a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2Zm0 3v12h8V5H8Zm4 13.25a1 1 0 1 0 0 2 1 1 0 0 0 0-2Z"/>',
  },
  web: {
    label: "Web",
    emoji: "🌐",
    twemoji: "1f310",
    icon: '<g fill="none" stroke="currentColor" stroke-width="2"><circle cx="12" cy="12" r="9"/><ellipse cx="12" cy="12" rx="3.8" ry="9"/><path d="M3.5 9h17M3.5 15h17"/></g>',
  },
  embedded: {
    label: "Console",
    emoji: "🎮",
    twemoji: "1f3ae",
    icon: '<path fill-rule="evenodd" d="M7.5 6h9a5.5 5.5 0 0 1 5.5 5.5v2a4.5 4.5 0 0 1-8 2.8l-.8-1.3h-2.4l-.8 1.3a4.5 4.5 0 0 1-8-2.8v-2A5.5 5.5 0 0 1 7.5 6ZM7 9v1.75H5.25v1.5H7V14h1.5v-1.75h1.75v-1.5H8.5V9H7Zm9.5.25a1 1 0 1 0 0 2 1 1 0 0 0 0-2Zm-2 2.5a1 1 0 1 0 0 2 1 1 0 0 0 0-2Z"/>',
  },
  vr: {
    label: "VR",
    emoji: "🥽",
    twemoji: "1f97d",
    icon: '<path fill-rule="evenodd" d="M4 7h16a2 2 0 0 1 2 2v5a2 2 0 0 1-2 2h-4.2l-2.4-2.4a2 2 0 0 0-2.8 0L8.2 16H4a2 2 0 0 1-2-2V9a2 2 0 0 1 2-2Zm3 3.5a1.5 1.5 0 1 0 0 3 1.5 1.5 0 0 0 0-3Zm10 0a1.5 1.5 0 1 0 0 3 1.5 1.5 0 0 0 0-3Z"/>',
  },
};

const STATUS: Record<Status, { label: string; color: number }> = {
  online: { label: "Online", color: 0xff23a55a },
  idle: { label: "Idle", color: 0xfff0b232 },
  dnd: { label: "Do Not Disturb", color: 0xfff23f43 },
};

const settings = phoenix.settings.define({
  memberList: {
    type: "switch",
    title: "Show in the member list",
    description: "Next to each member's name (desktop only)",
    default: true,
    onChange: (on) => syncMembers(on),
  },
  profileBadges: { type: "switch", title: "Profile badges", default: true },
  ignoreBots: { type: "switch", title: "Ignore bots", default: true, onChange: () => members?.update?.() },
  showSelf: { type: "switch", title: "Show my own", default: true, onChange: () => members?.update?.() },
});

const presences = new Map<string, Presence>();
const drawn = new Set<string>();
const bots = new Set<string>();
let selfId: string | undefined;

const isRecord = (v: unknown): v is Record<string, unknown> => typeof v === "object" && v !== null;
const isStatus = (v: unknown): v is Status => v === "online" || v === "idle" || v === "dnd";
const devices = (p: Presence) => (Object.keys(PLATFORMS) as Platform[]).filter((d) => p.platforms[d]);
const key = (p?: Presence) => (p ? devices(p).map((d) => d + p.platforms[d]).join() : "");
const svg = (body: string, color = "#000") =>
  `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="${color}" color="${color}">${body}</svg>`;
const hex = (argb: number) => `#${(argb & 0xffffff).toString(16).padStart(6, "0")}`;

function setPresence(userId: string, presence?: Presence) {
  const before = key(presences.get(userId));
  if (presence && devices(presence).length) {
    presences.delete(userId);
    presences.set(userId, presence);
    if (presences.size > 20_000) presences.delete(presences.keys().next().value!);
  } else {
    presences.delete(userId);
  }
  // Most updates only change activities, which leave the row as it is.
  if (key(presence) !== before && drawn.has(userId)) members?.update?.(userId);
}

function readPresence(raw: unknown, userId?: string) {
  if (!isRecord(raw)) return;
  const user = isRecord(raw.user) ? raw.user : {};
  const id = typeof user.id === "string" ? user.id : typeof raw.user_id === "string" ? raw.user_id : userId;
  if (!id) return;
  const platforms: Presence["platforms"] = {};
  for (const [device, status] of Object.entries(isRecord(raw.client_status) ? raw.client_status : {})) {
    if (device in PLATFORMS && isStatus(status)) platforms[device as Platform] = status;
  }
  setPresence(id, isStatus(raw.status) ? { status: raw.status, platforms } : undefined);
}

const readAll = (list: unknown) => Array.isArray(list) && list.forEach((raw) => readPresence(raw));
const on = (sig: Phoenix.Sig<unknown> | undefined, fn: (payload: any) => void) => sig && phoenix.on(sig, fn);
const { gateway } = phoenix;

on(gateway.presenceUpdate, readPresence);
on(gateway.presencesReplace, readAll);
on(gateway.guildMembersChunk, (p) => readAll(p?.presences));
on(gateway.readySupplemental, (p) => {
  readAll(p?.merged_presences?.friends);
  (p?.merged_presences?.guilds ?? []).forEach(readAll);
});
on(gateway.guildMemberListUpdate, (p) => {
  for (const op of p?.ops ?? []) {
    for (const item of op.items ?? [op.item]) {
      const user = item?.member?.user;
      if (!user?.id) continue;
      if (user.bot) bots.add(user.id);
      readPresence(item.member.presence, user.id);
    }
  }
});
// Your own devices are not in presence updates, only in your sessions.
on(gateway.sessionsReplace, (sessions) => {
  if (!selfId || !Array.isArray(sessions)) return;
  const platforms: Presence["platforms"] = {};
  let status: Status | undefined;
  for (const s of sessions) {
    if (!isStatus(s?.status)) continue;
    if (s.session_id === "all") status = s.status;
    else if (s.client_info?.client in PLATFORMS) platforms[s.client_info.client as Platform] ??= s.status;
  }
  setPresence(selfId, status && { status, platforms });
});
phoenix.on(gateway.messageCreate, (m) => m.author?.bot && bots.add(m.author.id));
phoenix.discord.me().then((me) => (selfId = me?.id)).catch(() => {});

function shown(userId: string, bot: boolean) {
  if ((bot && settings.ignoreBots) || (userId === selfId && !settings.showSelf)) return;
  return presences.get(userId);
}

let members: Phoenix.Remover | undefined;

function syncMembers(enabled: boolean) {
  if (enabled && !members) {
    members = phoenix.ui.addMemberDecoration({
      render: ({ userId }) => {
        if (drawn.size > 2_000) drawn.clear();
        drawn.add(userId);
        const p = shown(userId, bots.has(userId));
        if (!p) return null;
        return {
          text: devices(p).map((d) => PLATFORMS[d].emoji).join(" "),
          color: STATUS[p.status].color,
          icons: devices(p).map((d) => ({ svg: svg(PLATFORMS[d].icon), color: STATUS[p.platforms[d]!].color })),
        };
      },
    });
  } else if (!enabled && members) {
    members();
    members = undefined;
  }
}
syncMembers(settings.memberList);

const toHex = (s: string) => [...s].map((c) => c.charCodeAt(0).toString(16).padStart(2, "0")).join("");

// Phoenix caches badges by id, so every icon and label gets its own, clear of global-badges' ids.
function badgeId(text: string) {
  let hash = 2166136261;
  for (const c of text) hash = Math.imul(hash ^ c.charCodeAt(0), 16777619);
  return String(1_100_000_000 + ((hash >>> 0) % 900_000_000));
}

phoenix.discord.onResponse({ method: "GET", pathPrefix: "/users/", pathSuffix: "/profile" }, async (res) => {
  if (!settings.profileBadges || res.status < 200 || res.status >= 300) return;
  const match = res.path.match(/^\/users\/(@me|\d+)\/profile/);
  const userId = match?.[1] === "@me" ? (await phoenix.discord.me())?.id : match?.[1];
  const data = res.json<Record<string, any>>();
  if (!userId || !data) return;
  const p = shown(userId, data.user?.bot === true);
  if (!p) return;
  const badges = devices(p).map((d) => {
    const status = p.platforms[d]!;
    // Desktop serves our icon in the status color; elsewhere Twemoji stands in.
    const uri =
      phoenix.ui.iconUrl?.(svg(PLATFORMS[d].icon, hex(STATUS[status].color))) ??
      `https://cdn.jsdelivr.net/gh/jdecked/twemoji@15.1.0/assets/72x72/${PLATFORMS[d].twemoji}.png`;
    return {
      id: badgeId(`platform-indicators:${d}:${status}`),
      description: `${PLATFORMS[d].label} · ${STATUS[status].label}`,
      icon: `phx_u_${toHex(uri)}`,
      simple_icon_url: uri,
    };
  });
  data.badges = [...badges, ...(Array.isArray(data.badges) ? data.badges : [])];
  return { body: data };
});
