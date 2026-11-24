// Ending a session, from the reader's side.
//
// `signOut` in `api.ts` is the local half: it clears the stored credential
// and publishes the session-level fact so every tab returns to the gate.
// A password login's session is also a row at the core, and leaving it live
// after the reader walked away is a credential nobody remembers. So the
// sign-out controls come through here: revoke at the core, best-effort and
// briefly, then hand the credential back locally whatever the core said.
// An outage must not keep a reader signed in against their wish.
//
// The device's push subscription ends with the session (WI-924). Push was
// registered while signed in and outlived a sign-out, so a phone kept
// receiving notifications that opened onto a login screen: two lifecycles
// that had come apart. It is unsubscribed first, while the credential that
// may remove it still works, and best-effort like the revoke.

import { getToken, signOut } from "./api";
import { currentPushEnabled, unsubscribePushNotifications } from "./push";
import { logout } from "./vogtApi";

/** At most this long for each best-effort step, so leaving is never slow. */
const STEP_DEADLINE_MS = 2000;

async function bestEffort(step: () => Promise<unknown>): Promise<void> {
  try {
    await Promise.race([
      step(),
      new Promise<void>((resolve) => setTimeout(resolve, STEP_DEADLINE_MS)),
    ]);
  } catch {
    /* done or not, the reader is leaving */
  }
}

export async function signOutAndRevoke(detail = ""): Promise<void> {
  const token = getToken();
  if (token) {
    await bestEffort(async () => {
      if (await currentPushEnabled()) await unsubscribePushNotifications();
    });
    await bestEffort(() => logout("signed out from the browser"));
  }
  signOut(detail);
}
