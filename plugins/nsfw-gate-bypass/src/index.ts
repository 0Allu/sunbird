const { gateway } = plumage;

const VERIFIED_ADULT = 3;

interface Gated {
  nsfw_allowed?: boolean | null;
  age_verification_status?: number;
}

const unlock = (user: Gated | undefined) => {
  if (!user) return;
  user.nsfw_allowed = true;
  user.age_verification_status = VERIFIED_ADULT;
};

const ready = gateway.ready as Plumage.Sig<{ user?: Gated }> | undefined;
const userUpdate = gateway.userUpdate as Plumage.Sig<Gated> | undefined;

if (ready) plumage.hook(ready, { before: (payload) => unlock(payload.user) });
else plumage.log.warn("this build has no READY handler");
if (userUpdate) plumage.hook(userUpdate, { before: unlock });
