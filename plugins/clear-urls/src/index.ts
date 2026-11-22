// @description Removes tracking parameters from links in messages
// @version 0.1.0
// @author amaanq

const { gateway } = plumage;

const TRACKERS = [
  /^utm_/,
  /^(fbclid|gclid|dclid|gbraid|wbraid|msclkid|yclid|twclid|ttclid|igshid|igsh|mc_eid|mc_cid)$/,
  /^(si|feature|pp|ref_src|ref_url|s|t)$/,
  /^(_hsenc|_hsmi|mkt_tok|oly_anon_id|oly_enc_id|vero_id|wickedid|__s)$/,
];

// `s` and `t` are tracking only on these hosts. Elsewhere they are often real
// parameters, such as a YouTube start time.
const SHORT_PARAM_HOSTS = /(^|\.)(twitter\.com|x\.com|tiktok\.com)$/;

const clean = (url: string) => {
  const query = url.indexOf("?");
  if (query === -1) return url;
  const hash = url.indexOf("#", query);
  const end = hash === -1 ? url.length : hash;
  const host = url.slice(url.indexOf("//") + 2).split(/[/?#:]/)[0].toLowerCase();
  const kept = url
    .slice(query + 1, end)
    .split("&")
    .filter((pair) => {
      const key = decodeURIComponent(pair.split("=")[0]);
      if ((key === "s" || key === "t") && !SHORT_PARAM_HOSTS.test(host)) return true;
      return !TRACKERS.some((tracker) => tracker.test(key));
    });
  return url.slice(0, query) + (kept.length ? `?${kept.join("&")}` : "") + url.slice(end);
};

const rewrite = (message: { content?: string }) => {
  if (typeof message.content !== "string" || !message.content.includes("?")) return;
  message.content = message.content.replace(/https?:\/\/[^\s<>()]+/g, clean);
};

plumage.hook(gateway.messageCreate, { before: rewrite });
plumage.hook(gateway.messageUpdate, { before: rewrite });
