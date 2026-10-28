import { defineEnv, string } from 'void/env';

export default defineEnv({
  // Opens a room to stream from: only the owner's apps have it.
  HOST_KEY: string(),
  // Cloudflare's TURN relay, which the apps connect through: the key's ID
  // and API token, from the dashboard's Realtime TURN keys. Without them
  // the room offers no relay.
  TURN_KEY_ID: string().optional(),
  TURN_KEY_API_TOKEN: string().optional(),
});
