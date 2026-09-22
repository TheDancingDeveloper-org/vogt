// Ending a session, from the reader's side.
//
// `signOut` in `api.ts` is the local half: it clears the stored credential
// and publishes the session-level fact so every tab returns to the gate.
// A password login's session is also a row at the core, and leaving it live
// after the reader walked away is a credential nobody remembers. So the
// sign-out controls come through here: revoke at the core, best-effort and
// briefly, then hand the credential back locally whatever the core said.
// An outage must not keep a reader signed in against their wish.

import { getToken, signOut } from "./api";
import { logout } from "./vogtApi";

export async function signOutAndRevoke(detail = ""): Promise<void> {
  const token = getToken();
  if (token) {
    try {
      await Promise.race([
        logout("signed out from the browser"),
        new Promise<void>((resolve) => setTimeout(resolve, 2000)),
      ]);
    } catch {
      /* revoked at the core or not, the reader is leaving */
    }
  }
  signOut(detail);
}
