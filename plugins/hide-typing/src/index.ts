// @description Hides typing indicators
// @version 0.1.0
// @author amaanq

const typing = plumage.gateway.typingStart;

if (typing) plumage.hook(typing, { before: () => false });
else plumage.log.warn("this build has no TYPING_START handler");
