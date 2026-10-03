# Evelopment Games Designer Server

A small sync server for [Evelopment Games Designer](https://github.com/Evelynnsrepos/Evelopment-Games-Designer) (0.8 and later).
Projects are shared through the server, so people sync even if they are never online at the same time.
Run it on your own PC, on a rented server (VPS), or behind nginx for a whole studio.

- **No accounts.** The admin makes a **key** per person (view or edit) and sends them a **connect code**.
  Everyone types their own name in the app. Names are not checked.
- **One program.** No database server, no web server needed. Settings live in a web page on the server itself.
- **Private by default.** No IP addresses in logs, no tracking, no cookies except the admin login,
  nothing loaded from the internet. See [Privacy](#privacy).
- **Free software** under the GPL-3.0, like the app.

## Quick start (your own PC)

1. Download the server for your system from the releases page and start it (double-click on Windows and macOS).
2. Your browser opens the admin page (`http://127.0.0.1:8475/admin`). Pick a name and an admin password,
   and choose **On this computer or in my home network**.
3. Restart the server once so it uses that connection mode. On Windows, allow it in the firewall prompt.
4. **Projects → New project**, then **Keys → New key** for each person. Send each person their connect code.
5. In Evelopment Games Designer:
   - the person who already has the project opens it, chooses **Work together → Or work through a server**, pastes their code and presses **Move to server**;
   - everyone else chooses **Join project** on the start screen and pastes their code.

Teammates outside your home network need a port forward for port 8474 in your router, or use a VPS.

The Windows and macOS downloads show a tray icon (menu bar on macOS) with **Open admin page**, **Open data folder** and **Stop server**.

## On a VPS behind nginx

```bash
./egd-server --no-browser          # admin page on 127.0.0.1:8475, sync on 127.0.0.1:8474
ssh -L 8475:127.0.0.1:8475 you@your-server   # from your computer, then open http://127.0.0.1:8475/admin
```

In the wizard choose **Behind nginx or another web server** and enter `wss://your-domain/sync` as the address.
Settings shows a ready nginx snippet; it is also in [packaging/nginx.conf](packaging/nginx.conf).
Turn on **Behind a reverse proxy** in Settings so connection limits see real addresses.
A systemd unit is in [packaging/egd-server.service](packaging/egd-server.service).

Without nginx, choose **Public server with its own domain**: the server gets certificates from Let's Encrypt
itself (it needs port 443 and a domain pointing at it).

## Docker

```bash
docker build -t egd-server -f packaging/Dockerfile .
docker run -d --name egd-server -v egd-data:/data \
  -p 127.0.0.1:8475:8475 -p 8474:8474 --read-only egd-server
```

The admin port is published on 127.0.0.1 only. See [packaging/docker-compose.yml](packaging/docker-compose.yml).

## Command line

```
egd-server [--data <folder>] [--no-browser] [--headless] [--version]
```

| | |
|---|---|
| `--data` / `EGD_DATA` | Where projects and settings are kept. Default: `%LOCALAPPDATA%\egd-server`, `~/Library/Application Support/egd-server`, `~/.local/share/egd-server`. |
| `EGD_SYNC_ADDR` | Listen address for apps (default `127.0.0.1:8474` behind a proxy, `0.0.0.0:8474` otherwise, `0.0.0.0:443` with Let's Encrypt). |
| `EGD_ADMIN_ADDR` | Listen address of the admin page (default `127.0.0.1:8475`). Other addresses need TLS (self-signed or certificate mode), except when set here, which is meant for Docker. |

## Connection modes

| Mode | For | How apps trust the server |
|---|---|---|
| Self-signed | a PC or home network | the certificate's SHA-256 fingerprint is in every connect code and the app pins it |
| Behind a proxy | nginx, Caddy, Apache | your proxy's certificate, checked against the system's trust store |
| Certificate files | a certificate you already have | checked against the system's trust store |
| Let's Encrypt | a public server with a domain | checked against the system's trust store |

TLS 1.3 only (BSI TR-02102-2). Plain WebSocket is only ever served on 127.0.0.1, for a proxy in front.

## How it works

The server speaks the app's collaboration protocol (version 1, see [docs/PROTOCOL.md](docs/PROTOCOL.md)) over a WebSocket.
It is an always-on member of each project: it keeps the project's [Yjs](https://yjs.dev) document in SQLite,
relays edits and presence, and stores images and audio. The app's peer-to-peer mode keeps working as before.

- A **view** key can read and see who is online. The server drops any change it sends and disconnects it if it keeps trying.
- **Revoking** a key disconnects it at once. **Closing** a project disconnects everyone; they can keep a local copy.
- The server is the host: only the admin removes people or closes a project.
- Limits: 8 MiB per message, 200 MB per file, a storage quota per project (2 GB by default), 5 connections per key, 30 per IP address.

### Plugins

The admin can install plugins on the server (**Plugins** tab, the same zip the app installs).
Apps connected to the server are offered them. **Nothing is installed by itself**: every plugin goes through the
app's plugin warning, with a note that the server's admin chose it, and a changed plugin asks again.
Plugins are programs that run with the app's full rights on every computer that installs them,
so a server admin should only offer plugins they trust completely.

## Privacy

Whoever runs the server is responsible for the data in it (the *controller* under the GDPR). It stores:

- project content, until the project is deleted (optionally after N days unused);
- per key: a hash of the key, its label, view/edit, expiry, and the last name typed with it; deleted with the key;
- an admin log (what was done on the admin page, no IP addresses), 90 days;
- backups, 14 days.

It does **not** store IP addresses (held in memory only while connected), connection history or activity per person.
Project content is not end-to-end encrypted in 0.8: whoever runs the server can read it.
Use disk encryption (BitLocker, FileVault, LUKS) for the data and backup folders.

Templates for operators (privacy notice, records of processing, technical and organisational measures,
notes for companies and works councils) are in [docs/operators](docs/operators). They are templates, not legal advice.

## Building

```bash
cd server
cargo build --release                    # headless
cargo build --release --features tray    # with the tray icon (Windows, macOS)
cargo test
cd ../protocol && npm install && npm test   # end-to-end, with the app's own Yjs packages
```

## License

GPL-3.0-only. See [LICENSE](LICENSE). Security issues: see [SECURITY.md](SECURITY.md).
