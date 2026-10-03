# Security

## Reporting a vulnerability

Please report security problems privately through GitHub: **Security → Report a vulnerability** on this repository
(private vulnerability reporting). Do not open a public issue.

Include what is affected, how to reproduce it, and what an attacker could do. You get an answer as soon as possible;
this is a free project run by one person, so please allow some days.

Fixes are released as a new version, and the release notes say which changes are security fixes.

## Supported versions

Only the latest release gets security fixes.

## What the server does to stay safe

- TLS 1.3 only on the sync port (or plain WebSocket bound to 127.0.0.1 for a reverse proxy in front).
- Keys: 256 bits from the operating system's random source, stored only as SHA-256 hashes, checked before anything about a project is revealed.
- View keys are enforced on the server: changes and file uploads from them are dropped.
- Admin page: bound to 127.0.0.1 by default, Argon2id password hash, HttpOnly SameSite=Strict session cookie,
  CSRF token on every change, strict Content Security Policy, no external resources, login lockout after repeated failures.
- Every message from the network is size-limited and checked; unknown message types close the connection.
  Presence data is decoded with range checks, and a malformed message can only drop its own connection.
- Asset paths are checked against one strict pattern, so nothing can be read or written outside a project's folder.
- No IP addresses are written anywhere.

Known limits: project content is not end-to-end encrypted in 0.8, so whoever runs the server can read it.
Plugins offered by a server are programs; the app always asks before installing one.
