// Microsoft Teams transport.
//
// Posts a message to a Teams channel through an incoming webhook. The webhook
// URL identifies the target channel, so the mail recipients are not used here.
//
// The payload is an Adaptive Card wrapped in a message attachment, which is the
// format expected by Power Automate / Workflows webhooks (the successor to the
// retired Office 365 connector webhooks).
//
// Expected config fields:
//   webhook_url  - the Teams workflow webhook URL (store via webhook_url_env)

fn send(mail, config) {
    let card = #{
        type: "AdaptiveCard",
        "$schema": "http://adaptivecards.io/schemas/adaptive-card.json",
        version: "1.4",
        body: [
            #{ type: "TextBlock", text: mail.subject, weight: "Bolder", size: "Medium", wrap: true },
            #{ type: "TextBlock", text: mail.body, wrap: true },
        ],
    };

    let payload = #{
        type: "message",
        attachments: [
            #{
                contentType: "application/vnd.microsoft.card.adaptive",
                content: card,
            },
        ],
    };

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
