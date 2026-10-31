// A room where a Shuttercrab that is streaming and the Shuttercrabs that
// watch it swap the UDP addresses they can be reached at (issue #66):
// their addresses on Cloudflare's TURN relay, which the room hands them a
// login for, or for a direct connection, their own. Nothing else passes
// through here: the stream itself goes between the apps, end to end
// encrypted, and the key that proves who is streaming stays in the link,
// never sent to this Worker. No app is told another's own address.
//
// The one streaming opens the room with a host key (each person allowed
// to stream has their own); anyone else may join a room that is open.
import * as v from 'valibot';
import { defineRoom } from 'void/ws';
import { env } from 'void/env';
import { hostKeys, refusal } from '../../access';

/** An address as `ip:port`, `[ipv6]:port` included. */
const Address = v.pipe(v.string(), v.maxLength(64));

const ClientMessage = v.variant('type', [
  // Where this app can be reached, for the UDP socket it will connect
  // from: its relay address, or its own. None at first, while it waits
  // for its relay login.
  v.object({ type: v.literal('hello'), addresses: v.pipe(v.array(Address), v.maxLength(8)) }),
]);

const ServerMessage = v.variant('type', [
  // Another app in the room, and where it says it can be reached.
  v.object({
    type: v.literal('peer'),
    id: v.string(),
    host: v.boolean(),
    addresses: v.array(v.string()),
  }),
  v.object({ type: v.literal('left'), id: v.string() }),
  // A login to Cloudflare's TURN relay for this app, for as long as it
  // stays in the room.
  v.object({
    type: v.literal('relay'),
    urls: v.array(v.string()),
    username: v.string(),
    credential: v.string(),
  }),
]);

type Server = v.InferInput<typeof ServerMessage>;

/**
 * A connection: the host or a viewer, or one turned away, with the close
 * code and reason it gets; and the relay login it was given.
 */
type State = {
  host: boolean;
  addresses: string[];
  refused?: [number, string];
  relay?: string;
};

/** How long a relay login lasts: longer than any stream. */
const RELAY_TTL = 12 * 60 * 60;

const TURN_API = 'https://rtc.live.cloudflare.com/v1/turn/keys';

/** The TURN key's ID and token, if the room has them. */
function turnKey(): { id: string; token: string } | null {
  const id = env.TURN_KEY_ID?.trim();
  const token = env.TURN_KEY_API_TOKEN?.trim();
  return id && token ? { id, token } : null;
}

/** A new relay login from Cloudflare, or null without a TURN key. */
async function relayLogin(): Promise<Extract<Server, { type: 'relay' }> | null> {
  const key = turnKey();
  if (!key) {
    return null;
  }
  const response = await fetch(`${TURN_API}/${key.id}/credentials/generate-ice-servers`, {
    method: 'POST',
    headers: { Authorization: `Bearer ${key.token}`, 'Content-Type': 'application/json' },
    body: JSON.stringify({ ttl: RELAY_TTL }),
  });
  if (!response.ok) {
    console.error('no relay login', response.status);
    return null;
  }
  const { iceServers } = (await response.json()) as {
    iceServers: { urls: string[]; username?: string; credential?: string }[];
  };
  const turn = iceServers.find((s) => s.username && s.credential);
  return turn
    ? { type: 'relay', urls: turn.urls, username: turn.username!, credential: turn.credential! }
    : null;
}

/** End a relay login once its app has left. */
async function revoke(username: string) {
  const key = turnKey();
  if (key) {
    await fetch(`${TURN_API}/${key.id}/credentials/${encodeURIComponent(username)}/revoke`, {
      method: 'POST',
      headers: { Authorization: `Bearer ${key.token}` },
    });
  }
}

export default defineRoom({
  name: 'rooms',
  messages: { client: ClientMessage, server: ServerMessage },

  async onConnect(ctx) {
    const auth = ctx.request.headers.get('Authorization');
    const open = [...ctx.room.getConnections()].some(
      (c) => c.id !== ctx.connection.id && (c.state as State | undefined)?.host,
    );
    const refused = (await refusal(auth, open, hostKeys(env))) ?? undefined;
    const state: State = { host: auth !== null && !refused, addresses: [], refused };
    ctx.connection.setState(state);
  },

  // The first hello gets a relay login. Then tell the other side of the
  // room (the host, or its viewers) where this app is, and tell this app
  // where each of them is.
  async onMessage(ctx, event) {
    let me = ctx.connection.state as State;
    if (me.refused) {
      ctx.connection.close(...me.refused);
      return;
    }
    if (me.relay === undefined) {
      const login = await relayLogin();
      me = { ...me, relay: login?.username ?? '' };
      if (login) {
        await ctx.connection.send(login);
      }
    }
    me = { ...me, addresses: event.addresses };
    ctx.connection.setState(me);
    if (me.addresses.length === 0) {
      return;
    }
    const peer = (id: string, s: State): Server => ({
      type: 'peer',
      id,
      host: s.host,
      addresses: s.addresses,
    });
    for (const other of ctx.room.getConnections()) {
      const them = other.state as State | undefined;
      if (other.id === ctx.connection.id || !them || them.refused || them.host === me.host) {
        continue;
      }
      await other.send(peer(ctx.connection.id, me));
      if (them.addresses.length > 0) {
        await ctx.connection.send(peer(other.id, them));
      }
    }
  },

  // A viewer leaving tells the host; the host leaving closes the room.
  // Either way its relay login ends.
  async onClose(ctx) {
    const me = ctx.connection.state as State | undefined;
    if (!me || me.refused) {
      return;
    }
    if (me.relay) {
      await revoke(me.relay);
    }
    await ctx.room.broadcast({ type: 'left', id: ctx.connection.id }, [ctx.connection.id]);
    if (me.host) {
      for (const other of ctx.room.getConnections()) {
        if (other.id !== ctx.connection.id) {
          other.close(1000, 'The stream ended');
        }
      }
    }
  },
});
