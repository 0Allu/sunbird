// @description Sets the staff flag on your account to unlock experiments
// @version 0.1.0

const { gateway } = plumage;

const STAFF = 1;

interface Flagged {
  flags?: number;
}

const promote = (user: Flagged | undefined) => {
  if (user) user.flags = (user.flags ?? 0) | STAFF;
};

const ready = gateway.ready as Plumage.Sig<{ user?: Flagged }> | undefined;
const userUpdate = gateway.userUpdate as Plumage.Sig<Flagged> | undefined;

if (ready) plumage.hook(ready, { before: (payload) => promote(payload.user) });
else plumage.log.warn("this build has no READY handler");
if (userUpdate) plumage.hook(userUpdate, { before: promote });
