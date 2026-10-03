# Technische und organisatorische Maßnahmen (Art. 32 DSGVO) / Technical and organisational measures

> Vorlage, keine Rechtsberatung. „Server“ = was die Software selbst tut; „Betreiber“ = was Sie ergänzen.
> Template, not legal advice. "Server" = what the software does; "Operator" = what you add.

| Bereich / Area | Server | Betreiber / Operator |
|---|---|---|
| Verschlüsselung der Übertragung / Encryption in transit | TLS 1.3 (BSI TR-02102-2); selbstsignierte Zertifikate werden per Fingerabdruck geprüft; unverschlüsselt nur auf 127.0.0.1 hinter einem Proxy / TLS 1.3; self-signed certificates pinned by fingerprint; plain only on 127.0.0.1 behind a proxy | Proxy mit TLS 1.3 betreiben / run the proxy with TLS 1.3 |
| Verschlüsselung gespeicherter Daten / Encryption at rest | – | Datenträgerverschlüsselung (BitLocker, FileVault, LUKS) für Daten- und Sicherungsordner / disk encryption for the data and backup folders |
| Zugangskontrolle / Access control | Schlüssel mit 256 Bit, nur als Hash gespeichert; Lesen/Bearbeiten wird auf dem Server durchgesetzt; Widerruf trennt sofort; Ablaufdatum / 256-bit keys stored as hashes; view/edit enforced on the server; revoking disconnects at once; expiry | Ein Schlüssel pro Person; Schlüssel ausscheidender Personen widerrufen / one key per person; revoke keys of people who leave |
| Admin-Zugang / Admin access | Nur 127.0.0.1 (Fernzugriff nur mit TLS); Argon2id; Sperre nach Fehlversuchen; Sitzungs-Cookie HttpOnly, SameSite=Strict; CSRF-Schutz; strikte CSP / 127.0.0.1 only (remote only with TLS); Argon2id; lockout; HttpOnly SameSite=Strict cookie; CSRF protection; strict CSP | Langes Passwort; Zugriff per SSH-Tunnel / long password; access through an SSH tunnel |
| Datensparsamkeit / Data minimisation | Keine IP-Adressen auf Datenträgern, keine Verbindungs- oder Aktivitätsprotokolle, keine externen Anfragen / no IP addresses on disk, no connection or activity logs, no external requests | Proxy-Zugriffsprotokolle abschalten (`access_log off`) / turn off proxy access logs |
| Verfügbarkeit / Availability | Tägliche Sicherungen (14 Tage), Projekt-Sicherung und -Wiederherstellung / daily backups (14 days), per-project backup and restore | Sicherungen zusätzlich extern aufbewahren; Wiederherstellung testen / keep a copy elsewhere; test a restore |
| Belastbarkeit / Resilience | Größenlimits, Verbindungslimits pro Schlüssel und IP, Speicherkontingent pro Projekt, Zeitlimits; fehlerhafte Nachrichten trennen nur ihre Verbindung / size, connection and storage limits, timeouts; malformed messages only drop their own connection | – |
| Protokollierung / Logging | Admin-Protokoll ohne IP-Adressen, 90 Tage / admin log without IP addresses, 90 days | – |
| Löschung / Erasure | Projekt löschen, Schlüssel widerrufen (Name wird mitgelöscht), optional automatische Löschung ungenutzter Projekte / delete project, revoke key (name deleted with it), optional auto-delete of unused projects | Löschanfragen bearbeiten / handle erasure requests |
| Updates | Sicherheitskorrekturen in den Versionshinweisen gekennzeichnet / security fixes marked in release notes | Updates zeitnah einspielen / update promptly |
| Prozessisolation / Isolation | Läuft als normaler Benutzer; Docker-Image ohne Root, schreibgeschützt außer /data; systemd-Härtung / runs as a normal user; non-root, read-only Docker image; hardened systemd unit | Mitgelieferte Dateien nutzen / use the provided files |
