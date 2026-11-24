// @description Lets the app open NSFW channels without age verification
// @version 0.1.1
// @author amaanq

const { gateway } = phoenix;

const VERIFIED_ADULT = 3;

const unlock = (user: Phoenix.CurrentUser) => {
  user.nsfw_allowed = true;
  user.age_verification_status = VERIFIED_ADULT;
};

if (gateway.ready) phoenix.hook(gateway.ready, { before: (payload) => unlock(payload.user) });
else phoenix.log.warn("this build has no READY handler");
if (gateway.userUpdate) phoenix.hook(gateway.userUpdate, { before: unlock });
