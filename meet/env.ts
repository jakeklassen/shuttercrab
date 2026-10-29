import { defineEnv, string } from 'void/env';

export default defineEnv({
  // Open a room to stream from: a key each for the people allowed to
  // stream, one `name:key` a line (or separated by commas), so one can be
  // taken back alone. The name is only for the owner's own bookkeeping.
  HOST_KEYS: string().optional(),
  // The first key, from before there were several: still accepted.
  HOST_KEY: string().optional(),
  // Cloudflare's TURN relay, which the apps connect through: the key's ID
  // and API token, from the dashboard's Realtime TURN keys. Without them
  // the room offers no relay.
  TURN_KEY_ID: string().optional(),
  TURN_KEY_API_TOKEN: string().optional(),
});
