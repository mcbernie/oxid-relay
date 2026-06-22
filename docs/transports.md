# Architektur: Transports, Ingress und Plugin-System

Hält die Richtung für Ein- und Ausgang von Nachrichten fest.

## Datenfluss

```
Interner Dienst (auf internem Server)
        │   SMTP (Einlieferung im LAN)
        ▼
   OxidRelay  ──► Queue (SQLite) ──► Transport Layer
        │
        ▼
Externer Zielkanal (z. B. Office 365, Mailgun, ntfy, SMS, Teams)
```

OxidRelay sitzt zwischen internem Dienst und externem Zielserver. Der interne
Dienst kann Benachrichtigungen typischerweise per Mail/SMTP versenden und gibt
OxidRelay als Server an.

## Ingress (Eingang)

- **Kein Fokus auf HTTP-API.** Interne Dienste (Backup, Monitoring) nutzen in
  der Regel kein HTTP, sondern Mailversand. Das vorhandene `http-api`-Crate
  bleibt optional und nachrangig.
- **Primärer Eingang: SMTP-Annahme.** OxidRelay nimmt Mails per SMTP im LAN
  entgegen (Submission-Listener), legt sie in die Queue und versendet sie über
  einen konfigurierten Transport weiter.
- Authentifizierung/Absender-Erkennung am Eingang: siehe
  [authentication.md](authentication.md).

## Egress (Ausgang) — Transport-Abstraktion

Alle Zielkanäle implementieren den `Transport`-Trait aus `crates/core`. Der Core
kennt keine konkrete Implementierung; neue Kanäle kommen ohne Core-Änderung
hinzu.

Geplante Transports:

- **SMTP** (generisch, STARTTLS) — vorhanden (`transport-smtp`).
- **Microsoft Graph (REST)** — für Office 365 der bevorzugte Weg. Microsoft
  empfiehlt Graph `sendMail` und baut SMTP AUTH zunehmend ab. Office 365 also
  nicht per SMTP, sondern per Graph anbinden.
- **Mailgun (REST)**, **Amazon SES** — später.

Nicht-Mail-Kanäle (gleiche Abstraktion):

- **ntfy** (Push), **SMS-Anbieter** (diverse), **Microsoft Teams** (Webhook).

Die REST-basierten Transports (Graph, Mailgun, SES, ntfy, Teams) sind im Kern
HTTP-Aufrufe und werden als native Rust-Transports mit **reqwest** gebaut —
schnell und typsicher. `reqwest` ist damit ohnehin Egress-Kerndependency.

## Plugin-System (Vision)

Ziel: beliebige Weiterleitungsziele anbinden, ohne den Core anzufassen — bis hin
zu ntfy, SMS oder Teams.

Anforderung: cross-platform (Windows/Linux/macOS) und Möglichkeit, APIs
anzusprechen. Native dynamische Bibliotheken (`.so`/`.dll`/`.dylib`) scheiden
faktisch aus — Rust hat keine stabile ABI, plus Build pro Plattform.

Optionen mit Abwägung:

- **Rhai** — reines Rust, kein C-Toolchain, trivialer Cross-Compile, sauberste
  Bindung an Rust-Typen. Etwas langsamer. Beste Wahl, wenn problemloser
  Cross-Build im Vordergrund steht.
- **Lua** (z. B. `mlua`, gebündelt) — industrieweit am meisten erprobt (nginx,
  Redis, Neovim), schnell, kompiliert überall. Bringt eine C-Abhängigkeit mit.
- **Subprozess / Webhook** — Plugin als externer Prozess oder HTTP-Endpoint;
  bekommt JSON, nutzt eigene HTTP-Bibliothek. Sprachunabhängig, beste Isolation,
  aber Prozess-Overhead und externe Binaries im Deployment.
- **WASM/WASI** — saubere Sandbox, aber HTTP aus WASM heraus noch unreif; der
  Host müsste alles durchreichen. Mehr Aufwand.

API-Zugriff aus Skripten: Skript-Engines (Rhai/Lua) können selbst kein HTTP. Der
Host stellt eine kuratierte Funktion bereit (`http_get`/`http_post`), intern mit
`reqwest`. Das Skript ruft nur diese — kein roher System-/Netzwerkzugriff.

Tendenz/Empfehlung: erst statische Rust-Transports sauber bauen; für das
Plugin-System **Rhai + reqwest-Host-Funktion** (reines Rust, einfachster
Cross-Build). Subprozess-Plugins als Alternative für fremdsprachige Ziele.
Entscheidung bleibt bis Stufe 5 offen.

## Reihenfolge (Vorschlag)

1. Dispatcher/Worker: Queue → Transport → Status (Retry/Backoff).
2. Microsoft Graph Transport (Office 365).
3. SMTP-Ingress-Listener.
4. Weitere Transports (Mailgun, SES, ntfy, SMS, Teams).
5. Plugin-System.
