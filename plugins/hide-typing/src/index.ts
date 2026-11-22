// @description Hides typing indicators
// @version 0.1.0
// @author amaanq

const typing = phoenix.gateway.typingStart;

if (typing) phoenix.hook(typing, { before: () => false });
else phoenix.log.warn("this build has no TYPING_START handler");
