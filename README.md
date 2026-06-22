# OxidRelay

Setting up mail delivery for notifications is tedious for a lot of services.
You want to be told when something happens, but wiring each tool up to a mail
provider is repetitive work. Office 365 in particular is often needlessly
complex, and older services that work perfectly well and should not be replaced
frequently fail against it. Sometimes you do not even want a mail at all. Maybe
a Microsoft Teams message fits better, or a ticket in a helpdesk, or a push
notification.

OxidRelay handles all of that for you. A service submits a message once, over
plain SMTP on your LAN, and the relay takes care of delivery through whatever
channel you configured: Microsoft 365 via Graph, a Teams webhook, ntfy, or your
own plugin. It is cross-platform, simple to configure, and easy to extend.

This project is still in development. Issues and suggestions are welcome.

## How it works

A submitting service speaks ordinary SMTP to the relay on the local network.
The relay accepts the message, stores it durably in a local queue, and
acknowledges immediately. The submitter does not wait for the actual delivery.
A background dispatcher then delivers queued messages in parallel, retrying with
exponential backoff, through the transport selected for the sender.

```
Internal service (backup, monitoring, legacy app)
        |  SMTP (LAN)
        v
   OxidRelay  --->  Queue (SQLite)  --(ack)-->  fast return
        |
        v   background, parallel, with retry
   Dispatcher  --->  Transport
                       |-- Microsoft Graph (Office 365)
                       |-- SMTP
                       |-- Teams (webhook)
                       |-- ntfy (push)
                       \-- your own plugin
```

Key properties:

- Submission is decoupled from delivery. Once a message is queued, the relay
  owns delivery and retries until it succeeds or is permanently dead.
- Delivery runs concurrently, so one slow target does not block others.
- Transports are pluggable. Mail-style and notification-style channels share the
  same abstraction.
- Routing decides, per sender, which channel is used and who receives the
  message. A sender can also be rejected outright.
- The relay is intended for LAN use only. Never expose it to the public
  internet.

## Building

Requires a recent Rust toolchain (edition 2024, Rust 1.85 or newer).

```
cargo build --release
```

The binary is `target/release/oxid-relay`.

## Running

```
oxid-relay --config config.toml
```

- The config path can also come from the `OXID_RELAY_CONFIG` environment
  variable. It defaults to `config.toml`.
- In debug builds (`cargo run`), a local `.env` file is loaded automatically so
  secrets are available without exporting them by hand. In release builds the
  process environment is used as-is.
- The dispatcher runs until the process receives Ctrl-C.

Secrets are never written into the configuration file. A field whose name ends
in `_env` holds the name of an environment variable; the value is read from
that variable at runtime.

The process shuts down cleanly on SIGINT (Ctrl-C) and, on Unix, on SIGTERM
(sent by service managers on stop).

## Test builds

Until there is a release workflow, the CI run on `main` attaches a bundle per OS
(`oxid-relay-windows-latest`, `-ubuntu-latest`, `-macos-latest`) to the workflow
run under the Actions tab. Each contains the release binary, the bundled
`plugins/`, `config.example.toml` and a short README. Download, extract, then:

```
# point the relay at the bundled plugins (release builds do not use ./plugins)
export OXID_RELAY_PLUGIN_DIR="$PWD/plugins"   # PowerShell: $env:OXID_RELAY_PLUGIN_DIR = "$PWD\plugins"
cp config.example.toml config.toml            # then edit
./oxid-relay --config config.toml
```

These are unsigned test builds, not releases.

## Running as a service

Run exactly one instance per queue. A second instance against the same queue
database refuses to start (an exclusive lock guards it).

### Linux (systemd)

A unit file is provided in `packaging/systemd/oxid-relay.service`.

```
sudo useradd --system --home /var/lib/oxid-relay --shell /usr/sbin/nologin oxid-relay
sudo install -m 0755 target/release/oxid-relay /usr/local/bin/oxid-relay
sudo install -d /etc/oxid-relay
sudo install -m 0640 config.toml /etc/oxid-relay/config.toml
# Secrets, mode 0600:
printf 'CLIENT_SECRET_AZURE=...\nTEAMS_WEBHOOK_URL=...\n' | sudo tee /etc/oxid-relay/oxid-relay.env >/dev/null
sudo chmod 0600 /etc/oxid-relay/oxid-relay.env
sudo cp packaging/systemd/oxid-relay.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now oxid-relay
journalctl -u oxid-relay -f
```

Release builds do not auto-load `.env`; the unit reads secrets from
`/etc/oxid-relay/oxid-relay.env`. The queue and its lock live under
`/var/lib/oxid-relay`; plugins are looked up in `/etc/oxid-relay/plugins`.

### macOS (launchd)

A LaunchDaemon plist is provided in `packaging/launchd/com.oxid-relay.plist`.

```
sudo cp target/release/oxid-relay /usr/local/bin/oxid-relay
sudo mkdir -p /usr/local/etc/oxid-relay /usr/local/var/lib/oxid-relay
sudo cp config.toml /usr/local/etc/oxid-relay/config.toml
sudo cp packaging/launchd/com.oxid-relay.plist /Library/LaunchDaemons/
sudo launchctl load /Library/LaunchDaemons/com.oxid-relay.plist
```

### Windows

The binary runs as a Windows service when started with `--service` (the service
control manager passes this). An install script is in
`packaging/windows/install-service.ps1`. From an elevated PowerShell, after
adjusting the paths inside it:

```
powershell -ExecutionPolicy Bypass -File packaging\windows\install-service.ps1
```

It registers the service with `binPath` pointing at the executable plus
`--service --config <path>`. Secrets are read from machine-level environment
variables (release builds do not load `.env`); set them with
`[Environment]::SetEnvironmentVariable(name, value, 'Machine')` and restart the
service. Note that service stdout is not captured yet; Windows Event Log / file
logging is planned. An MSI installer (cargo-wix) is also planned.

## Configuration

OxidRelay reads a single TOML file. A documented example lives in
`config.example.toml`. The sections below cover each part.

### Queue

```toml
[queue]
database = "queue.db"
```

Path to the SQLite database file. It is created if it does not exist.

### Logging

```toml
[logging]
level = "info"   # trace, debug, info, warn, error
```

`RUST_LOG` overrides this when set.

### Dispatcher

Tuning for the background delivery loop. All values are optional and default as
shown; times are in seconds.

```toml
[dispatcher]
batch_size = 64          # mails fetched per poll
concurrency = 8          # parallel deliveries
poll_interval_secs = 5   # delay between polls
sending_lease_secs = 120 # in-flight mail older than this is retried (self-heal)
max_attempts = 5         # attempts before a mail is buried as dead
retry_base_secs = 30     # first retry delay (exponential backoff)
retry_max_secs = 3600    # backoff cap
```

### Outgoing default transport

```toml
[mail]
default_transport = "graph"
```

Transport used for messages that do not name one. When unset, SMTP is the
default if an SMTP transport is configured. If you use routing (below), the
route normally decides the transport instead.

### SMTP transport (optional)

A built-in transport for any STARTTLS-capable SMTP server, including Office 365
SMTP. The password is read from the named environment variable.

```toml
[mail.smtp]
host = "smtp.office365.com"
port = 587
username = "relay@example.com"
password_env = "MAIL_PASSWORD"
```

Note that Microsoft is phasing out SMTP AUTH; for Office 365 the Graph plugin is
the recommended path.

### SMTP ingress

Makes the relay accept mail on the LAN. The presence of this section enables it.

```toml
[ingress.smtp]
bind = "0.0.0.0:2525"
hostname = "oxid-relay"
```

Version 1 accepts anonymous submission (mode A); enable `[auth.anonymous]`
below. AUTH LOGIN/PLAIN requires STARTTLS and is planned for a later release.

### Security

```toml
[security]
ip_whitelist = ["10.42.202.183", "10.0.0.0/8"]
```

Mandatory access control for the ingress. Entries are single IPs or CIDR
ranges. An empty or missing whitelist allows nothing.

### Sender identification and subject prefix

In anonymous mode the sender name is taken from the subject using a regular
expression with a named capture group `name`. The relay then prepends a label
to the subject.

```toml
[auth.anonymous]
enabled = true
subject_match = '^(?P<name>[^:]+):'

[subject]
format = "[Abs: %name%] %original%"
```

`%name%` is the resolved sender name, `%original%` is the full incoming subject.
With the example above, a subject `Server 187: disk full` becomes
`[Abs: Server 187] Server 187: disk full`. Per-sender format overrides are
possible:

```toml
[subject.senders."Server 187"]
format = "<%name%> %original%"
```

### Routing

Routing decides, per sender, which transport handles the message and optionally
overrides the recipients. It is active as soon as at least one rule or a default
is configured.

```toml
[routing]

# Applied to senders without a specific rule. Omit to reject unknown senders.
[routing.default]
transport = "graph"

# Send mail from this sender through Teams instead.
[routing.senders."alerts@teams.local"]
transport = "teams"
recipients = ["ops-channel@teams.local"]

# Refuse a specific sender.
[routing.senders."noreply@blocked.local"]
transport = "reject"

# Fan out one sender to several channels at once.
[routing.senders."alarm@monitoring.local"]
targets = [
    { transport = "teams", recipients = ["ops-channel@teams.local"] },
    { transport = "ntfy" },
    { transport = "graph", recipients = ["oncall@example.com"] },
]
```

Resolution order: a matching per-sender rule wins over the default; a missing
rule with no default, or `transport = "reject"`, refuses the message with SMTP
550 at submission time (nothing is queued).

A rule may instead list `targets` to fan out to several channels. Each target
becomes its own queue entry, delivered and retried independently, and may
override the recipients. A `"reject"` target is skipped.

Sender keys and recipient addresses must be valid e-mail addresses with a domain
part, for example `alerts@teams.local` rather than `alerts@teams`. The sending
service must use the same address in its SMTP `MAIL FROM` for the rule to match.

### Plugins

Each plugin reads a flat table of settings, passed to its script as the
`config` map. Keys ending in `_env` are resolved from the named environment
variable and exposed without the suffix (so `client_secret_env` becomes
`client_secret`). Secrets must always use the `_env` form.

```toml
[plugins.graph]
tenant_id = "00000000-0000-0000-0000-000000000000"
client_id = "11111111-1111-1111-1111-111111111111"
client_secret_env = "GRAPH_CLIENT_SECRET"
sender = "relay@example.com"

[plugins.teams]
webhook_url_env = "TEAMS_WEBHOOK_URL"

[plugins.ntfy]
topic = "oxidrelay-alerts"
server = "https://ntfy.sh"
# token_env = "NTFY_TOKEN"
```

## Office 365 via Graph

The bundled `graph` plugin delivers through the Microsoft Graph `sendMail`
endpoint using the OAuth2 client credentials flow.

1. Register an application in Azure (Entra ID). Note the directory (tenant) ID
   and the application (client) ID.
2. Create a client secret and store its value in the environment variable named
   by `client_secret_env` (for example `GRAPH_CLIENT_SECRET`). Store the value,
   not the secret ID.
3. Under API permissions, add the Microsoft Graph application permission
   `Mail.Send` and grant admin consent. No custom app roles are needed.
4. Set `sender` to the mailbox the relay sends as.

For least privilege, restrict which mailboxes the app may send as with an
Exchange Online `ApplicationAccessPolicy`. Otherwise the application can send as
any mailbox in the tenant.

## Developing a plugin

A plugin is a directory containing a manifest and a script. The relay scans the
plugin directories at startup, loads each plugin, and exposes it as a transport
named after the manifest.

Plugin directories:

- `OXID_RELAY_PLUGIN_DIR` (if set): overrides everything. One or more paths,
  separated by the platform path separator. Useful for trying out a downloaded
  build against a local `plugins` folder.
- Debug builds: `./plugins` (relative to the working directory).
- Linux: `/etc/oxid-relay/plugins`
- macOS: `/Library/Application Support/OxidRelay/plugins`
- Windows: `%PROGRAMDATA%\OxidRelay\plugins`

### Manifest

`plugin.toml`:

```toml
name = "myplugin"          # also the transport name used in routing
version = "0.1.0"
kind = "transport"
capabilities = ["send"]
description = "What this plugin does."
entry = "script.rhai"      # optional, defaults to script.rhai
```

### Script

The script is written in [Rhai](https://rhai.rs) and must define
`fn send(mail, config)`. It returns normally on success and throws to signal a
failure (which the dispatcher records and retries).

The `mail` map has these fields:

- `mail.from`   - `#{ email, name }` (name may be absent)
- `mail.to`     - array of `#{ email, name }`
- `mail.subject`
- `mail.body`   - plain text

The `config` map holds the resolved `[plugins.<name>]` settings (with `_env`
keys already resolved to their secret values).

### Host API

Scripts cannot touch the network directly. The host provides a small, curated
API:

- `http_get(url, headers)` returns `#{ status, body }`
- `http_post(url, headers, body)` returns `#{ status, body }`
- `http_post_form(url, headers, form)` returns `#{ status, body }` (sends a
  form-urlencoded body)
- `to_json(value)` returns a JSON string
- `parse_json(string)` returns a value
- `log_info(message)`

`headers` and `form` are object maps of string keys to string values.

### Examples

The bundled plugins are the best reference:

- `plugins/ntfy` is the simplest. It builds a URL from `server` and `topic`,
  sets the subject as the `Title` header, sends the body with `http_post`, and
  checks for a 2xx status. It also shows optional config handling with the `in`
  operator (`if "token" in config { ... }`).

- `plugins/teams` posts a small JSON payload to an incoming webhook. It shows
  building an object with `#{ ... }` and serialising it with `to_json`.

- `plugins/graph` is the most complete. It performs an OAuth2 client credentials
  request with `http_post_form`, parses the token from the JSON response with
  `parse_json`, builds the Graph `sendMail` payload, and posts it with a bearer
  token header.

A minimal plugin:

```rhai
fn send(mail, config) {
    let res = http_post(
        config.webhook_url,
        #{ "Content-Type": "application/json" },
        to_json(#{ text: `${mail.subject}\n\n${mail.body}` }),
    );
    if res.status < 200 || res.status >= 300 {
        throw `delivery failed: ${res.status} ${res.body}`;
    }
    log_info("delivered");
}
```

To use it: drop the directory into a plugin path, add a `[plugins.myplugin]`
section with its settings, and route a sender to it with
`transport = "myplugin"`.

## Status and roadmap

OxidRelay is under heavy development and is not production ready. Expect changes.

Tested so far:

- SMTP ingress (anonymous, IP whitelist), the queue, the parallel dispatcher with
  retry, routing, the subject prefix, and the plugin system (graph, teams, ntfy)
  are covered by automated tests.
- Only macOS has been used so far. Linux and Windows builds and behaviour are not
  yet verified.

Not yet tested:

- The outbound SMTP transport is implemented but has not been tested against a
  real SMTP server.

Not yet implemented:

- SMTP ingress authentication (AUTH LOGIN/PLAIN) and STARTTLS. Submission is
  currently anonymous, protected only by the IP whitelist. The auth logic
  (modes B1/B2) is prepared but not wired up.

Available:

- Single-instance lock on the queue, clean shutdown on SIGINT/SIGTERM, and
  service integration for Linux (systemd), macOS (launchd) and Windows (service
  control manager).
- Dispatcher tuning via configuration (concurrency, poll interval, retry
  backoff, attempt limit).
- GitHub Actions CI building and testing on Linux, macOS and Windows.

Planned:

- STARTTLS plus AUTH LOGIN/PLAIN for the ingress.
- MSI installer (cargo-wix) and Windows Event Log / file logging.
- GitHub Actions release workflow (binaries, installers).
- Additional transports and providers (for example Mailgun, Amazon SES, SMS).

All of this is still to come. Contributions and issues are welcome.

## License

MIT or Apache-2.0.
