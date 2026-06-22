// Microsoft Teams transport.
//
// Posts a message to a Teams channel through an incoming webhook. The webhook
// URL identifies the target channel, so the mail recipients are not used here.
//
// Expected config fields:
//   webhook_url  - the Teams incoming webhook URL (store via webhook_url_env)

fn send(mail, config) {
    let text = `${mail.subject}\n\n${mail.body}`;
    let payload = #{ text: text };

    let res = http_post(
        config.webhook_url,
        #{ "Content-Type": "application/json" },
        to_json(payload),
    );
    if res.status < 200 || res.status >= 300 {
        throw `teams webhook failed: ${res.status} ${res.body}`;
    }
    log_info(`teams: message posted (status ${res.status})`);
}
