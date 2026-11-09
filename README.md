# Evelopment Games Designer Server

A small sync server for [Evelopment Games Designer](https://github.com/Evelynnsrepos/Evelopment-Games-Designer) (0.8 and later).
Projects are shared through the server, so people sync even if they are never online at the same time.
Run it on your own PC, on a rented server (VPS), or behind nginx for a whole studio.

- **No accounts.** The admin makes a **key** per person (view, comment or edit) and sends them a **connect code**.
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

Teammates outside your home network need a port forward for port 8474 in your router, or use a VPS,
or [Tailscale](#on-your-own-pc-day-to-day) (no port forward).

The Windows and macOS downloads show a tray icon (menu bar on macOS) with **Open admin page**, **Open data folder** and **Stop server**.

## On your own PC, day to day

You don't need a server that is always on. The server can start and stop by itself:

```bash
egd-server autostart on     # once; `egd-server autostart off` undoes it
```

- **Linux:** a systemd user socket holds the ports and starts the server on the first connection (and so also right after login).
  With **Settings → Stop when nobody was connected for N minutes**, it stops when idle and starts again when someone connects.
  Stop a copy you started by hand first, so the ports are free. User services stop when you log out; `loginctl enable-linger $USER` keeps them.
- **macOS / Windows:** it starts when you log in. Idle it uses almost nothing (projects are unloaded when nobody is connected).
  Idle stop works too, but then it only comes back at the next login or when you start it.
- **Before it stops** (idle stop, Ctrl+C, the tray's Stop server, the PC shutting down) it saves every project and pushes the last changes to git.
- **From outside your home network** without a port forward or VPS: install [Tailscale](https://tailscale.com) (free for personal use) on this PC
  and on your teammates' devices, choose **On this computer or in my home network** in the wizard, and enter the PC's Tailscale name as the address.
- When the PC is off, everyone keeps working in the app; it syncs when the server is back.

## Git sync

The server can keep a git history of each project in the background (**Projects → Git sync**).

- It writes the project as the **normal project folder** the app saves (`project.json`, `entities/`, `components/`, `assets/`)
  and pushes it every 10 minutes when something changed (Settings), when the server stops, and on **Sync now**.
  Clone the repository and open the folder in the app to get a copy. Sketch drawings stay on each computer, as in the app.
- **Push only.** It never pulls and never forces: if someone else pushed to the branch, it stops and says so.
  Use one repository per project, and don't also turn on the app's Git backup for the same repository.
- It uses the **git installed on this computer and its login** (SSH key, Git Credential Manager, a saved login).
  Log in once with git as the user the server runs as. The server stores no password or token; addresses with one are refused.
- **Private repositories only.** Before every push it checks that the repository can't be read without a login.
  A public repository needs **Allow a public repository**, a red warning and typing the project's name, and shows a red **Public** tag.
- Files over 100 MB stop the push (GitHub refuses them).

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

## Headless, command line only

Everything the admin page does also works as a command, so a server without a screen or browser
never needs the admin page. Commands work on the data folder whether the server runs or not;
a running server picks changes up within a few seconds (closed or deleted projects, revoked keys, a new password).

```bash
echo 'a long admin password' | egd-server setup --mode proxy --name "Studio" --public-url wss://egd.example.com/sync
egd-server project new "Moon Game"
egd-server key new "Moon Game" Ana              # prints Ana's connect code (only once)
egd-server key new "Moon Game" Ben --view --expires-days 30
egd-server git set "Moon Game" git@github.com:studio/moon-game.git
egd-server settings set dailyBackups=true idleStopMin=0
egd-server --no-browser --headless --no-admin-page     # run it
```

`egd-server --help` lists every command: `status`, `projects`, `project new|rename|storage|close|reopen|delete|backup|restore`,
`keys`, `key new|revoke`, `git`, `git set|off|sync`, `settings`, `settings set`, `password`, `plugins`, `plugin install|remove`,
`backup`, `log`, `autostart on|off`. Add `--json` for machine-readable output. P is a project's name or id.
The admin password is read from `EGD_ADMIN_PASSWORD` or the first line of stdin.
`--no-admin-page` turns the admin page off completely.

In Docker: `docker exec -i egd-server egd-server projects` (the image has `EGD_DATA` set and includes git for git sync).
With the systemd unit: `sudo -u egd egd-server --data /var/lib/egd-server projects`.

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
egd-server autostart on|off      # start when you log in (Linux: on the first connection)
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
- A **comment** key can read everything and write review comments and statuses, nothing else. The server tries each change on a copy of the project first and drops anything outside the Reviews document.
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
- per key: a hash of the key, its label, view/comment/edit, expiry, and the last name typed with it; deleted with the key;
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
