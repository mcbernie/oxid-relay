// Microsoft Graph transport.
//
// Delivers a mail through Microsoft 365 using the Graph API. Authentication
// uses the OAuth2 client credentials flow; delivery uses the sendMail endpoint.
//
// Expected config fields:
//   tenant_id, client_id, client_secret  - Azure app registration
//   sender                               - mailbox the mail is sent from

fn send(mail, config) {
    // 1. Acquire an access token (client credentials grant).
    let token_url = `https://login.microsoftonline.com/${config.tenant_id}/oauth2/v2.0/token`;
    let token_form = #{
        client_id: config.client_id,
        client_secret: config.client_secret,
        scope: "https://graph.microsoft.com/.default",
        grant_type: "client_credentials",
    };

    let token_res = http_post_form(token_url, #{}, token_form);
    if token_res.status != 200 {
        throw `token request failed: ${token_res.status} ${token_res.body}`;
    }
    let access_token = parse_json(token_res.body).access_token;

    // 2. Build the sendMail payload from the mail.
    let recipients = [];
    for recipient in mail.to {
        recipients.push(#{ emailAddress: #{ address: recipient.email } });
    }

    let payload = #{
        message: #{
            subject: mail.subject,
            body: #{ contentType: "Text", content: mail.body },
            toRecipients: recipients,
        },
        saveToSentItems: false,
    };

    // 3. Send the mail via the Graph sendMail endpoint.
    let send_url = `https://graph.microsoft.com/v1.0/users/${config.sender}/sendMail`;
    let headers = #{
        "Authorization": `Bearer ${access_token}`,
        "Content-Type": "application/json",
    };

    let res = http_post(send_url, headers, to_json(payload));
    if res.status != 202 {
        throw `sendMail failed: ${res.status} ${res.body}`;
    }

    log_info(`graph: delivered mail to ${mail.to.len()} recipient(s)`);
}
