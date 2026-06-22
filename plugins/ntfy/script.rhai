// ntfy transport.
//
// Publishes a push notification to an ntfy topic. The body becomes the message,
// the subject becomes the notification title.
//
// Expected config fields:
//   topic   - the ntfy topic name (required)
//   server  - base URL, defaults to https://ntfy.sh
//   token   - optional access token for protected topics (store via token_env)

fn send(mail, config) {
    let server = if "server" in config { config.server } else { "https://ntfy.sh" };
    let url = `${server}/${config.topic}`;

    let headers = #{ "Title": mail.subject };
    if "token" in config {
        headers["Authorization"] = `Bearer ${config.token}`;
    }

    let res = http_post(url, headers, mail.body);
    if res.status < 200 || res.status >= 300 {
        throw `ntfy failed: ${res.status} ${res.body}`;
    }
    log_info(`ntfy: published to ${config.topic} (status ${res.status})`);
}
