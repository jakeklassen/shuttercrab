// A room where a Shuttercrab that is streaming and the Shuttercrabs that
// watch it swap the UDP addresses they can be reached at, so they can
// connect directly (issue #66). Nothing else passes through here: the
// stream itself goes between the apps, and the key that proves who is
// streaming stays in the link, never sent to this Worker.
//
// The one streaming opens the room with the host key (only the owner's
// apps have it); anyone else may join a room that is open.
import * as v from 'valibot';
import { defineRoom } from 'void/ws';
import { env } from 'void/env';

/** An address as `ip:port`, `[ipv6]:port` included. */
const Address = v.pipe(v.string(), v.maxLength(64));

const ClientMessage = v.variant('type', [
  // Where this app can be reached: its LAN addresses and the public one
  // STUN reported, all for the UDP socket it will connect from.
  v.object({ type: v.literal('hello'), addresses: v.pipe(v.array(Address), v.maxLength(8)) }),
]);

const ServerMessage = v.variant('type', [
  // Another app in the room, and where it says it can be reached. `seen`
  // is the address its socket to this room came from.
  v.object({
    type: v.literal('peer'),
    id: v.string(),
    host: v.boolean(),
    addresses: v.array(v.string()),
    seen: v.string(),
  }),
  v.object({ type: v.literal('left'), id: v.string() }),
]);

type Server = v.InferInput<typeof ServerMessage>;

/**
 * A connection: the host or a viewer, or one turned away, with the close
 * code and reason it gets.
 */
type State = { host: boolean; addresses: string[]; seen: string; refused?: [number, string] };

/** Whether `given` is the host key, taking the same time either way. */
async function isHostKey(given: string): Promise<boolean> {
  const digest = async (text: string) =>
    crypto.subtle.digest('SHA-256', new TextEncoder().encode(text));
  const [a, b] = await Promise.all([digest(given), digest(env.HOST_KEY)]);
  return crypto.subtle.timingSafeEqual(a, b);
}

/**
 * Why a connection may not join, if it may not: as a close code and a
 * reason, which the apps show. It is closed in answer to its first
 * message. (Refusing the upgrade itself would be plainer, but the local
 * Workers runtime crashes on a refused upgrade, and loses a close sent
 * while connecting.)
 */
async function refusal(auth: string | null, open: boolean): Promise<[number, string] | null> {
  if (auth !== null) {
    if (!auth.startsWith('Bearer ') || !(await isHostKey(auth.slice(7)))) {
      return [4401, 'Not the host key'];
    }
    return open ? [4409, 'Someone is already streaming here'] : null;
  }
  return open ? null : [4404, 'No one is streaming here'];
}

export default defineRoom({
  name: 'rooms',
  messages: { client: ClientMessage, server: ServerMessage },

  async onConnect(ctx) {
    const auth = ctx.request.headers.get('Authorization');
    const open = [...ctx.room.getConnections()].some(
      (c) => c.id !== ctx.connection.id && (c.state as State | undefined)?.host,
    );
    const refused = (await refusal(auth, open)) ?? undefined;
    const state: State = {
      host: auth !== null && !refused,
      addresses: [],
      seen: ctx.request.headers.get('CF-Connecting-IP') ?? '',
      refused,
    };
    ctx.connection.setState(state);
  },

  // Tell the other side of the room (the host, or its viewers) where
  // this app is, and tell this app where each of them is.
  async onMessage(ctx, event) {
    const me = ctx.connection.state as State;
    if (me.refused) {
      ctx.connection.close(...me.refused);
      return;
    }
    ctx.connection.setState({ ...me, addresses: event.addresses });
    const peer = (id: string, s: State): Server => ({
      type: 'peer',
      id,
      host: s.host,
      addresses: s.addresses,
      seen: s.seen,
    });
    for (const other of ctx.room.getConnections()) {
      const them = other.state as State | undefined;
      if (other.id === ctx.connection.id || !them || them.refused || them.host === me.host) {
        continue;
      }
      await other.send(peer(ctx.connection.id, { ...me, addresses: event.addresses }));
      if (them.addresses.length > 0) {
        await ctx.connection.send(peer(other.id, them));
      }
    }
  },

  // A viewer leaving tells the host; the host leaving closes the room.
  async onClose(ctx) {
    const me = ctx.connection.state as State | undefined;
    if (!me || me.refused) {
      return;
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
