const typing = plumage.gateway.typingStart;

if (typing) plumage.hook(typing, { before: () => false });
else plumage.log.warn("this build has no TYPING_START handler");
