import { defineConfig } from 'void/config';

export default defineConfig({
  worker: {
    compatibility_date: '2026-02-24',
  },
  cloudflare: {
    name: 'shuttercrab-meet',
    // The rooms: one Durable Object per routes/rooms/[room].ws.ts room.
    durable_objects: {
      bindings: [{ name: 'RoomsWs', class_name: 'RoomsWs' }],
    },
    migrations: [{ tag: 'void-ws-v1', new_sqlite_classes: ['RoomsWs'] }],
  },
});
