# Feature: Authentifizierung & Absender-Erkennung

Legt fest, wie ein Dienst sich beim Relay anmeldet und wie daraus der
Absender-Name (`%name%`, siehe [subject-prefix.md](subject-prefix.md)) bestimmt
wird.

## Betriebsgrundsatz

- Das Relay wird **niemals im öffentlichen Raum (WAN / Internet)** betrieben,
  ausschließlich intern im LAN.
- Eine **IP-Whitelist ist in jedem Modus Pflicht**. Nur freigegebene Quell-IPs
  dürfen Nachrichten einliefern.

## Modi

Jeder Dienst, der per Mail/SMTP benachrichtigen kann, gibt das Relay als Ziel
an. Drei Wege der Identifikation:

### Modus A — anonym (keine Zugangsdaten)

- Zugang nur über IP-Whitelist.
- Absender-Identifikation über den **Betreff-Inhalt**: ein Muster im Betreff
  wird ausgewertet und einem `%name%` zugeordnet.

### Modus B1 — Benutzername/Passwort, vorab in Config

- Pro Dienst ein Credential-Block in der Config mit fester Namensvergabe.
- Benutzername/Passwort wurden vorher im Relay festgelegt.
- `%name%` stammt aus der Config-Zuordnung.
- IP-Whitelist zusätzlich Pflicht.

### Modus B2 — Benutzername/Passwort zur Selbst-Erkennung

- Zugangsdaten existieren **nicht vorab** in der Config.
- Die übergebenen Credentials dienen dazu, den Dienst zu erkennen; `%name%`
  wird aus dem Benutzernamen abgeleitet (Selbst-Registrierung bei Erstkontakt).
- IP-Whitelist zusätzlich Pflicht.

## TOML-Entwurf

```toml
[security]
# Relay is LAN-only. Whitelist is mandatory in every mode.
ip_whitelist = ["10.0.0.0/8", "192.168.0.0/16"]

# --- Modus A: anonyme Einlieferung, Name aus Betreff ---
[auth.anonymous]
enabled = true
# Pattern to extract the sender name from the subject.
# Capture group / placeholder yields %name%.
subject_match = "^(?P<name>Server ?\\d+):"

# --- Modus B1: feste Credentials pro Dienst ---
[auth.services."Server01.company.local"]
username = "server01"
password_env = "SERVER01_PASSWORD"

[auth.services."backup-host"]
username = "backup"
password_env = "BACKUP_PASSWORD"

# --- Modus B2: Selbst-Erkennung, Name aus Username ---
[auth.self_register]
enabled = true
# %name% derived from the supplied username.
```

## Offene Punkte

- **Modus B2 Sicherheit:** Selbst-Registrierung mit beliebigen Credentials ist
  riskant. Absichern über IP-Whitelist (Pflicht) — ggf. zusätzlich gemeinsames
  Vorab-Secret oder Freigabe-Schritt erwägen.
- **Mehrdeutigkeit:** Was gilt, wenn mehrere Modi gleichzeitig zutreffen?
  Vorschlag: B1 vor B2 vor A.
- **Passwort-Ablage:** nur über `password_env` / Secret-Store, nie Klartext in
  der Config.
- **Betreff-Match Modus A:** Verhalten, wenn das Muster nicht greift —
  ablehnen oder Fallback-Name?
- **Transport-Protokoll:** liefert der Dienst per SMTP (lettre als Server-Seite
  nötig) oder über die HTTP-API ein? Beides möglich, Auth-Modell gilt für beide.

## Verortung im Code

- Auth-/Identifikationslogik als Geschäftslogik in `crates/core` (Trait
  `SenderResolver` o.ä.), testbar ohne Netzwerk.
- IP-Whitelist-Prüfung am Eingang (`http-api`, später SMTP-Listener).
- Config-Laden im Binary, Übergabe an den Core.
