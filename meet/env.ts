import { defineEnv, string } from 'void/env';

export default defineEnv({
  // Opens a room to stream from: only the owner's apps have it.
  HOST_KEY: string(),
});
