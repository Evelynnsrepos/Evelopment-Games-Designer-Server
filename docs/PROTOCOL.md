# Protocol

The server speaks the app's collaboration protocol **version 1** (`src/core/collab/protocol.ts` in the app) over a WebSocket at `/sync`.
This page describes only what is added around it.

## Connect codes

`EGS1-` + base64url of a JSON object:

| Field | |
|---|---|
| `u` | WebSocket address, e.g. `wss://games.example.com/sync` |
| `f` | SHA-256 of the server's certificate (lowercase hex) in self-signed mode, otherwise `null`. The app pins it. |
| `p` | The project's id on the server |
| `n` | Project name |
| `s` | Server name |
| `k` | The key, `egd-key-` + 43 base64url characters (256 random bits) |

The code is personal. The app keeps it in the project's `collab/server.json`, never in the shared Yjs state.

## Opening a connection

1. The client opens the WebSocket and sends one **text** message: `{"type":"auth","key":"egd-key-..."}` (within 10 seconds).
2. The server answers with one text message:
   - `{"type":"auth-ok","serverId","serverName","projectId","serverProjectId","projectName","role","empty"}`
     - `projectId` is the **app's** id for the project (its meta id), or `null` while the server project is empty.
       The first change uploaded binds the server project to the uploader's project id; hellos with another id are rejected with `project`.
     - `role` is `view` or `write`. `empty` is true while nothing has been uploaded.
   - or `{"type":"auth-error","reason"}` with `bad-key` (unknown or revoked), `expired`, `closed` (project closed), `busy` (too many connections for this key), `bad-request`, and the connection closes.
3. From then on, **binary** messages are protocol v1 frames (`[type u8][payload]`), unchanged:
   hello, welcome, reject, Yjs sync, awareness, asset request, asset chunk, closed.
   - The device id in the client's hello must be `k-` + the first 32 hex digits of SHA-256(key).
   - The server's hello uses its own `serverId`, schema of the project (set by the first app to connect) and the client's `projectId`.
   - The server never publishes presence of its own.
   - The server asks write connections for asset files that documents mention and it lacks; files are only accepted when requested, and only from write connections.
4. **Text** messages after authentication are control messages:
   - `{"type":"plugins"}` → `{"type":"plugins","plugins":[{id,name,version,description,sha256,size,added}]}`
   - `{"type":"plugin","id"}` → `{"type":"plugin","id","sha256","zip":"<base64url>"}` or `{"type":"plugin","id","missing":true}`

## Ending

- Revoking a key: the server sends v1 `reject` with reason `not-member`, then closes. The next connection gets `auth-error` `bad-key`.
- Closing or deleting a project, or restoring a backup: v1 `closed` with reason `host-closed`, then the connection closes.
- The server pings every 30 seconds and drops connections silent for 90 seconds.

## Limits

8 MiB per message, 512 KB asset chunks, 200 MB per file, a storage quota per project, 5 connections per key, 30 per IP address,
64 presence entries per update and 16 presence client ids per connection.
View connections that send changes three times are disconnected.
