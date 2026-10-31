# Live streaming

Shuttercrab can stream an area, a window or a display, with its sound, to
someone anywhere. They watch in Shuttercrab too, from a link you send them.
It is Windows only for now (issue #66).

## How it works

- **The meeting point** is a small Cloudflare Worker, in `meet/`, live at
  `https://shuttercrab.sandbox.gdn`. It introduces a streamer to a viewer
  and hands both a login for Cloudflare's relay (TURN). It never sees the
  stream.
- **The relay** carries the stream between the two PCs. Going through it
  always means it works on VPNs and behind any router, with nothing to
  open on either side.
- **The link** (`shuttercrab://watch/…`) names the room and carries a
  secret that only the viewer with the link has. Whoever has the link can
  watch, so send it only to the person meant to watch.
- **The pictures** go as AV1, HEVC or H.264, the best both PCs handle, in
  10 bits where they can; the sound goes as Opus. The streamer chooses
  1080p, 1440p or 4K; 1080p is lightest for both PCs.

## Streaming

1. Choose **Stream** (L) in the main window or the Capture Bar
   (Ctrl+Alt+C), or **Stream an area…** in the tray menu.
2. The first time, Shuttercrab asks for your **host key** (below). Copy it,
   then **Paste host key** (or Ctrl+V) on the Stream page, or **Paste key**
   in Settings, Streaming.
3. Choose an area, a window or a display, then **New**. Pick the area as for
   a recording.
4. The stream's bar offers the sound switches and the quality (1, 2, 3).
   **Go live** (Enter) opens the room and puts the link on the clipboard.
5. Send the link. The bar says "waiting for a viewer", then "Live" with the
   time, the codec, the quality and the delay there and back. **Copy link**
   (C) copies it again; **Stop** (S) ends the stream. Closing a streamed
   window ends it too.

One viewer at a time for now.

## Watching

Watching needs no key and no account. Click the `shuttercrab://` link, or
choose **Watch** (J) in the main window or the Capture Bar, or **Watch a
stream…** in the tray, and press Enter: the box fills itself from the
clipboard when a link is there.

The viewer opens at once, saying "Joining the stream…" until the pictures
come, or why it could not join. Its taskbar icon is the app's with the
colours swapped, coral, so it stands apart from the app's.

| Key | Does |
|---|---|
| **Up**, **Down**, or the mouse wheel | Volume, in steps of 10%, from 0 to 400%: above 100% the sound is raised, and a limiter keeps it from clipping. The viewer remembers it for next time |
| **M** | Mute, and again to unmute; turning the volume up unmutes too |
| **[** and **]** | Show the pictures 20 ms earlier or later against the sound |
| **Escape** | Close the viewer |

When the stream ends, the viewer says so and closes after 10 seconds. When
testing on one PC, mute the viewer or the sound plays twice.

The volume a viewer hears starts from the level the streamer's apps play
at, before the streamer's Windows volume. So for the streamer, it is best
to keep apps' own volume sliders high and turn the Windows volume down to
taste.

## The host key

The host key is a password for the meeting point: only someone with a key
can open a stream there, so strangers cannot stream through it and use up
its relay. Watching never needs one.

Shuttercrab keeps the key encrypted to your Windows account (DPAPI) in its
settings. It is never shown or logged. **Forget** in Settings, Streaming
removes it.

The meeting point has other secrets of its own, `TURN_KEY_ID` and
`TURN_KEY_API_TOKEN`, Cloudflare's login for the relay. They are **not**
host keys and never go into Shuttercrab.

## Running the meeting point

The meeting point's owner makes and uploads every key; nobody else ever
needs to see one. Its secrets live in `meet/.env`, which git ignores, and in
the Worker, uploaded with `void secret put`.

| Secret | What it is |
|---|---|
| `HOST_KEY` | The first host key: the owner's |
| `HOST_KEYS` | One key per person who streams, as `name:key`, separated by commas or one a line. The name is only for the owner's own bookkeeping |
| `TURN_KEY_ID`, `TURN_KEY_API_TOKEN` | Cloudflare's Realtime TURN key, from the dashboard. Without them the room offers no relay |

Every key is accepted, from either list. Run these from PowerShell, in the
repository root.

**Make a key.** This puts a new one on the clipboard without showing it:

```powershell
$b = [byte[]]::new(32); [Security.Cryptography.RandomNumberGenerator]::Fill($b); [Convert]::ToHexString($b).ToLower() | Set-Clipboard
```

**Give someone a key.** Paste it into `meet/.env` on the `HOST_KEYS` line
with their name, after anyone already there:

```
HOST_KEYS=sam:<key>,alex:<key>
```

Then upload the whole line, which replaces the list the Worker has, and send
them their key privately:

```powershell
(Get-Content meet\.env | Where-Object { $_ -match '^HOST_KEYS=' }) -replace '^HOST_KEYS=', '' | pnpm -C meet exec void secret put HOST_KEYS
```

**Take a key back.** Delete that person's `name:key` from the line and
upload it again. Nobody else's key changes.

**Change your own key.** Make a new one, put it after `HOST_KEY=` in
`meet/.env`, upload it, and paste it into Shuttercrab:

```powershell
(Get-Content meet\.env | Where-Object { $_ -match '^HOST_KEY=' }) -replace '^HOST_KEY=', '' | pnpm -C meet exec void secret put HOST_KEY
```

**Deploy a change to the Worker's code:**

```powershell
pnpm -C meet run check
pnpm -C meet exec void deploy
```

The Worker refuses a wrong key with exactly the words "Not the host key";
the app recognises them (`NOT_THE_HOST_KEY` in `shuttercrab-connect`), so
keep them as they are.

## When something goes wrong

| What you see | What it means |
|---|---|
| "Streaming needs your host key" | No key is kept yet: paste yours |
| "The meeting point didn't accept that host key" | The key is not one the meeting point knows: the wrong secret was copied, part of it, or a new key not yet uploaded. Shuttercrab forgets it; paste the right one |
| "The stream didn't start: …" | Anything else, with the reason. The log has more |
| "Could not join the stream: No one is streaming here" (viewer) | The stream has ended, or the link is old |

Logs are in `%LOCALAPPDATA%\Shuttercrab\logs`: `shuttercrab.log` for the
app and its streaming process, `viewer.log` for the viewer.
