// @description Translates messages in chosen channels with Kagi or Google
// @version 0.1.0
// @author amaanq

const { gateway } = plumage;

const settings = plumage.settings.define({
  channels: {
    type: "text",
    title: "Channels",
    description: "Channel IDs to translate, separated by commas",
    default: "",
  },
  target: {
    type: "text",
    title: "Target language",
    description: "Language code to translate into, like en or de",
    default: "en",
  },
  kagiToken: {
    type: "secret",
    title: "Kagi session token",
    description: "The token from the Session Link in kagi.com/settings/user_details. Empty uses Google",
    default: "",
  },
  kagiBest: {
    type: "switch",
    title: "Kagi best quality",
    description: "Better translations for about 10 times the Kagi usage of standard",
    default: false,
  },
});

interface Translation {
  text: string;
  from: string;
  label: string;
}

type Translator = (text: string, target: string) => Promise<Translation>;

const google: Translator = async (text, target) => {
  const query = Object.entries({
    "params.client": "gtx",
    dataTypes: "TRANSLATION",
    key: "AIzaSyDLEeFI5OtFBwYBIoK_jj5m32rZK5CkCXA",
    "query.sourceLanguage": "auto",
    "query.targetLanguage": target,
    "query.text": text,
  })
    .map(([name, value]) => `${name}=${encodeURIComponent(value)}`)
    .join("&");
  const response = await plumage.fetch(`https://translate-pa.googleapis.com/v1/translate?${query}`);
  if (!response.ok) throw new Error(`Google answered ${response.status}`);

  const { translation, sourceLanguage } = response.json<{ translation: string; sourceLanguage: string }>();
  return { text: translation, from: sourceLanguage, label: sourceLanguage };
};

const kagi =
  (token: string): Translator =>
  async (text, target) => {
    const response = await plumage.fetch(
      `https://translate.kagi.com/api/translate?token=${encodeURIComponent(token)}`,
      {
        method: "POST",
        body: {
          text,
          source_lang: "auto",
          target_lang: target,
          skip_definition: true,
          model: settings.kagiBest ? "best" : "standard",
        },
      },
    );
    switch (response.status) {
      case 200:
        break;
      case 429:
        plumage.log.warn("Kagi quota exceeded, using Google");
        return google(text, target);
      case 401:
      case 403:
        throw new Error("Kagi rejected the session token");
      default:
        throw new Error(`Kagi answered ${response.status}: ${response.text()}`);
    }

    const { translation, detected_language: detected } = response.json<{
      translation: string;
      detected_language?: { iso: string; label: string };
    }>();
    return { text: translation, from: detected?.iso ?? "", label: detected?.label ?? "" };
  };

const baseLanguage = (code: string) => code.split("-")[0].toLowerCase();

const subtext = (text: string) =>
  text
    .split("\n")
    .map((line) => `-# ${line}`)
    .join("\n");

plumage.on(gateway.messageCreate, async (message) => {
  const original = message.content?.trim();
  const channels = settings.channels.split(",").map((id) => id.trim());
  if (!original || !channels.includes(message.channel_id)) return;

  const target = settings.target.trim() || "en";
  const token = settings.kagiToken.trim();
  const translate = token ? kagi(token) : google;

  let translation: Translation;
  try {
    translation = await translate(original, target);
  } catch (err) {
    plumage.log.warn(`translating ${message.id}: ${err}`);
    return;
  }
  if (baseLanguage(translation.from) === baseLanguage(target)) return;
  if (translation.text.trim() === original) return;

  plumage.enqueue(gateway.messageUpdate, {
    id: message.id,
    channel_id: message.channel_id,
    ...(message.guild_id ? { guild_id: message.guild_id } : {}),
    content: `${message.content}\n${subtext(translation.text)} (${translation.label})`,
  });
});
