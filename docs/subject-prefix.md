# Feature: Subject Prefix (Absender-Kennzeichnung)

## Idee

Mehrere interne Server nutzen OxidRelay als zentrale Versandinstanz. Da am Ende
alles über ein einzelnes Mailkonto versendet wird, geht die Information verloren,
welcher Server eine Nachricht ausgelöst hat.

OxidRelay erweitert deshalb automatisch den Betreff jeder Nachricht um eine
konfigurierbare Absender-Kennzeichnung.

## Beispiel

Ein Server sendet über das Relay mit dem Betreff:

```
Server 187: Status Okay
```

Das Relay stellt eine Kennzeichnung voran:

```
[Abs: Server01.company.local] Server 187: Status Okay
```

## Konfiguration

### Format-String

Der Inhalt der Kennzeichnung ist über einen Format-String konfigurierbar.
Platzhalter werden zur Laufzeit ersetzt.

Platzhalter:

- `%name%` — Name des Absenders (Identität des sendenden Servers)
- `%original%` — ursprünglicher Betreff, wie vom Absender geschickt

Beispiel-Format:

```
[Abs: %name%] %original%
```

### Zwei Ebenen

1. **Global** — ein Standard-Format-String, der für alle Absender gilt.
2. **Pro Absender** — ein Absender kann ein eigenes Format überschreiben.

Pro-Absender-Format hat Vorrang vor dem globalen Format.

### TOML-Entwurf

```toml
[subject]
# Global default applied to every sender.
format = "[Abs: %name%] %original%"

# Per-sender override. Key is the sender name/identity.
[subject.senders."Server01.company.local"]
format = "[%name%] %original%"

[subject.senders."backup-host"]
format = "[Backup %name%] %original%"
```

## Offene Punkte

- Woran wird der Absender erkannt? Siehe [authentication.md](authentication.md)
  (IP-Whitelist + anonym/Betreff, feste Credentials oder Selbst-Erkennung).
- Verhalten, wenn `%original%` im Format fehlt — Betreff verwerfen oder anhängen?
- Mehrfach-Präfix verhindern, falls eine Mail erneut durchs Relay läuft
  (Idempotenz-Marker / bereits-präfixt-Erkennung).
- Maximale Betrefflänge / Kürzung beachten.

## Verortung im Code

- Format-Logik gehört in `crates/core` (reine Geschäftslogik, testbar ohne
  Transport/DB).
- Konfiguration (`[subject]`) wird vom Binary geladen und an den Core übergeben.

## Umsetzung (v1)

- `SubjectConfig::render(name, original)` in `crates/core` ersetzt `%name%` und
  `%original%`. `%original%` ist immer der vollständige eingehende Betreff.
- Der SMTP-Ingress wendet den Prefix beim Enqueue an. Modus A (anonym) ermittelt
  den Namen aus dem Betreff über `auth.anonymous.subject_match` (Regex mit
  benannter Gruppe `name`). Greift das Muster nicht, bleibt der Betreff
  unverändert.
- Hinweis: Steckt der Name im Betreff (Modus A), erscheint er dank `%original%`
  doppelt (z. B. `[Abs: Server 187] Server 187: ...`). Über Muster und Format
  steuerbar. Für authentifizierte Absender (Modus B, mit AUTH/TLS) kommt der
  Name aus den Zugangsdaten, ohne Dopplung.
