use reqwest::Client;
use serde::Serialize;
use tracing::{info, warn};

/// Payload representing a parsed email to forward to Discord.
pub struct EmailPayload {
    pub from: String,
    pub subject: String,
    pub date: String,
    pub body: String,
    pub account_name: String,
}

#[derive(Serialize)]
struct DiscordWebhook {
    embeds: Vec<DiscordEmbed>,
}

#[derive(Serialize)]
struct DiscordEmbed {
    title: String,
    description: String,
    color: u32,
    fields: Vec<EmbedField>,
    footer: EmbedFooter,
}

#[derive(Serialize)]
struct EmbedField {
    name: String,
    value: String,
    inline: bool,
}

#[derive(Serialize)]
struct EmbedFooter {
    text: String,
}

const MAX_BODY_LEN: usize = 4000;
const MAX_RETRIES: u32 = 3;

/// Send an email payload to a Discord webhook as a rich embed.
pub async fn send_to_discord(
    client: &Client,
    webhook_url: &str,
    payload: &EmailPayload,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let body = if payload.body.len() > MAX_BODY_LEN {
        let truncated = truncate(&payload.body, MAX_BODY_LEN);
        truncated
    } else {
        payload.body.clone()
    };

    let webhook = DiscordWebhook {
        embeds: vec![DiscordEmbed {
            title: if payload.subject.is_empty() {
                "(no subject)".to_string()
            } else {
                truncate(&payload.subject, 256)
            },
            description: body,
            color: 0x5865F2, // Discord blurple
            fields: vec![
                EmbedField {
                    name: "From".to_string(),
                    value: truncate(&payload.from, 1024),
                    inline: true,
                },
                EmbedField {
                    name: "Date".to_string(),
                    value: truncate(&payload.date, 1024),
                    inline: true,
                },
            ],
            footer: EmbedFooter {
                text: format!("via {}", payload.account_name),
            },
        }],
    };

    for attempt in 0..MAX_RETRIES {
        let resp = client.post(webhook_url).json(&webhook).send().await?;
        let status = resp.status();

        if status.is_success() || status == 204 {
            info!(
                account = %payload.account_name,
                subject = %payload.subject,
                "Email forwarded to Discord"
            );
            return Ok(());
        }

        if status == 429 {
            // Rate limited — extract retry_after from response body
            let body_text = resp.text().await.unwrap_or_default();
            let wait_secs: f64 = serde_json::from_str::<serde_json::Value>(&body_text)
                .ok()
                .and_then(|v| v["retry_after"].as_f64())
                .unwrap_or(2.0);

            warn!(
                attempt,
                retry_after_secs = wait_secs,
                "Discord rate limited, retrying"
            );
            tokio::time::sleep(std::time::Duration::from_secs_f64(wait_secs)).await;
            continue;
        }

        let error_body = resp.text().await.unwrap_or_default();
        return Err(format!("Discord webhook failed ({}): {}", status, error_body).into());
    }

    Err("Discord webhook failed after max retries (rate limited)".into())
}

fn truncate(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        s.to_string()
    } else {
        // Find the last char boundary at or before max_bytes - 3 (room for "…")
        let limit = max_bytes.saturating_sub(3);
        let end = s
            .char_indices()
            .take_while(|(i, _)| *i <= limit)
            .last()
            .map(|(i, c)| i + c.len_utf8())
            .unwrap_or(0);
        format!("{}…", &s[..end])
    }
}
