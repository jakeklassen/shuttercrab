// Who may join a room: the host keys, and the close code and reason a
// connection that may not join is turned away with. Apart from the Worker's
// environment, so the tests can give these their own made-up keys.

/** Where the host keys are kept: the Worker's `HOST_KEYS` and `HOST_KEY`. */
export type KeyEnv = { HOST_KEYS?: string; HOST_KEY?: string };

/**
 * The host keys: each line or comma-separated entry of `HOST_KEYS`, as
 * `name:key` or a bare key, and `HOST_KEY`. Blank entries are skipped, and
 * the stored text may end in the newline it was piped in with.
 */
export function hostKeys(env: KeyEnv): string[] {
  const entries = (env.HOST_KEYS ?? '').split(/[\n,]/).map((entry) => {
    const colon = entry.indexOf(':');
    return (colon >= 0 ? entry.slice(colon + 1) : entry).trim();
  });
  return [...entries, env.HOST_KEY?.trim() ?? ''].filter((key) => key.length > 0);
}

/**
 * Whether `given` is one of the host keys, comparing it with every one, so
 * it takes the same time whichever matches.
 */
async function isHostKey(given: string, keys: string[]): Promise<boolean> {
  const digest = async (text: string) =>
    crypto.subtle.digest('SHA-256', new TextEncoder().encode(text));
  const theirs = await digest(given);
  let found = false;
  for (const key of keys) {
    found = crypto.subtle.timingSafeEqual(theirs, await digest(key)) || found;
  }
  return found;
}

/**
 * Why a connection may not join, if it may not: as a close code and a
 * reason, which the apps show. It is closed in answer to its first
 * message. (Refusing the upgrade itself would be plainer, but the local
 * Workers runtime crashes on a refused upgrade, and loses a close sent
 * while connecting.)
 */
export async function refusal(
  auth: string | null,
  open: boolean,
  keys: string[],
): Promise<[number, string] | null> {
  if (auth !== null) {
    if (!auth.startsWith('Bearer ') || !(await isHostKey(auth.slice(7), keys))) {
      // The app knows this reason by its words (shuttercrab-connect's
      // NOT_THE_HOST_KEY): keep them as they are.
      return [4401, 'Not the host key'];
    }
    return open ? [4409, 'Someone is already streaming here'] : null;
  }
  return open ? null : [4404, 'No one is streaming here'];
}
