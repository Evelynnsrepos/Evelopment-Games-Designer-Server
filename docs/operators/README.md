# For people who run a server

> **Templates, not legal advice.** They describe what the software does and give you a starting point.
> If you run the server for a company or for the public, have them checked.
> *Vorlagen, keine Rechtsberatung.*

Whoever runs an Evelopment server decides what it is used for, so they are the **controller** (Verantwortlicher)
under the GDPR/DSGVO for the personal data in it. The people who make Evelopment only publish the software
and never see your data.

## What the server stores

| Data | Personal? | Kept |
|---|---|---|
| Project content (text, images, audio) | can contain personal data | until the project is deleted, or after N days unused if you turn that on |
| Key hash, label, view/edit, expiry | the label usually names a person | until the key is revoked or expires |
| Last name typed with each key | yes (self-chosen) | deleted with the key |
| Admin log (what was done on the admin page) | no IP addresses | 90 days |
| Backups | everything above | 14 days |
| IP addresses | yes | **never stored**; held in memory while connected, for connection limits |

No cookies for app users. The admin page sets one session cookie, which is strictly necessary
(§ 25 Abs. 2 Nr. 2 TDDDG), so no cookie banner is needed. The server makes no requests to the internet,
except to Let's Encrypt in that mode.

## Which situation are you in?

**Only for yourself (or friends, privately).** The GDPR's household exemption may apply.
On a VPS: keep the admin page on 127.0.0.1 and use an SSH tunnel, pick a hoster in the EU,
and sign the hoster's data processing agreement (AVV) if they offer one.

**For a team or company.** You need:
- a privacy notice for the people who use it: generate one on the admin page (**Privacy & legal**), or start from
  [datenschutzhinweise.md](datenschutzhinweise.md) / [privacy-notice.md](privacy-notice.md);
- an entry in your records of processing: [verzeichnis-verarbeitungstaetigkeiten.md](verzeichnis-verarbeitungstaetigkeiten.md);
- your technical and organisational measures: [toms.md](toms.md);
- the notes for employers: [unternehmen.md](unternehmen.md) (employee data, works council, hoster contract).

**Public (anyone can get a key).** Additionally: an Impressum (§ 5 DDG), terms of use, a contact point and a way to report
illegal content (Digital Services Act), and a plan for data breaches (report within 72 hours, Art. 33 GDPR).

## Good defaults

- Keep the admin page on 127.0.0.1. Use a long password.
- One key per person, with an expiry for guests. Revoke keys of people who leave.
- Turn on daily backups and keep the data and backup folders on an encrypted disk (BitLocker, FileVault, LUKS).
- Behind nginx: keep `access_log off` for the sync location, as in [packaging/nginx.conf](../../packaging/nginx.conf).
- Update the server when a new release says it contains security fixes.
