use crate::config::AccountConfig;
use crate::discord::{send_to_discord, EmailPayload};
use crate::oauth2::OAuth2Manager;
use crate::state::ForwardedState;

use reqwest::Client;
use serde::Deserialize;
use std::sync::Arc;
use std::time::Duration;
use tracing::{error, info, warn};

const GRAPH_MESSAGES_URL: &str = "https://graph.microsoft.com/v1.0/me/messages";
const POLL_INTERVAL: Duration = Duration::from_secs(30);
const MAX_BACKOFF_SECS: u64 = 300;

#[derive(Debug, Deserialize)]
struct GraphResponse {
    value: Vec<GraphMessage>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphMessage {
    id: String,
    subject: Option<String>,
    received_date_time: Option<String>,
    from: Option<GraphFrom>,
    body: Option<GraphBody>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphFrom {
    email_address: GraphEmail,
}

#[derive(Debug, Deserialize)]
struct GraphEmail {
    name: Option<String>,
    address: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphBody {
    content: Option<String>,
    content_type: Option<String>,
}

/// Main listener loop for a Microsoft Graph API account.
/// Polls for new unread emails and forwards them to Discord.
pub async fn listen(
    account: AccountConfig,
    state: ForwardedState,
    oauth2: Arc<OAuth2Manager>,
) {
    let http_client = Client::new();
    let mut backoff = 1u64;

    loop {
        match poll_and_forward(&account, &http_client, &state, &oauth2).await {
            Ok(()) => {
                backoff = 1;
            }
            Err(e) => {
                error!(
                    account = %account.name,
                    error = %e,
                    backoff_secs = backoff,
                    "Graph API poll error"
                );
                tokio::time::sleep(Duration::from_secs(backoff)).await;
                backoff = (backoff * 2).min(MAX_BACKOFF_SECS);
                continue;
            }
        }

        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

async fn poll_and_forward(
    account: &AccountConfig,
    http_client: &Client,
    state: &ForwardedState,
    oauth2: &Arc<OAuth2Manager>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let client_id = account.client_id.as_deref().unwrap_or("");
    let access_token = oauth2
        .get_access_token(&account.name, client_id, &account.oauth2_scope)
        .await?;

    // Fetch unread messages from all folders
    let resp = http_client
        .get(GRAPH_MESSAGES_URL)
        .bearer_auth(&access_token)
        .query(&[
            ("$filter", "isRead eq false"),
            ("$select", "id,subject,receivedDateTime,from,body"),
            ("$top", "50"),
            ("$orderby", "receivedDateTime desc"),
        ])
        .send()
        .await?;

    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("Graph API error ({}): {}", status, body).into());
    }

    let graph_resp: GraphResponse = resp.json().await?;

    if graph_resp.value.is_empty() {
        return Ok(());
    }

    info!(
        account = %account.name,
        count = graph_resp.value.len(),
        "Found unread messages via Graph API"
    );

    for msg in &graph_resp.value {
        // Check local state — skip if already forwarded
        if state.is_forwarded(&account.name, "graph", &msg.id).await {
            continue;
        }

        let from = msg
            .from
            .as_ref()
            .map(|f| {
                let name = f.email_address.name.as_deref().unwrap_or("");
                let addr = f.email_address.address.as_deref().unwrap_or("unknown");
                if name.is_empty() {
                    addr.to_string()
                } else {
                    format!("{} <{}>", name, addr)
                }
            })
            .unwrap_or_else(|| "unknown".to_string());

        let subject = msg
            .subject
            .as_deref()
            .unwrap_or("(no subject)")
            .to_string();

        let date = msg
            .received_date_time
            .as_deref()
            .unwrap_or("unknown")
            .to_string();

        // Convert HTML body to plain text if needed
        let body_text = msg
            .body
            .as_ref()
            .and_then(|b| {
                let content = b.content.as_deref().unwrap_or("");
                if content.is_empty() {
                    return None;
                }
                if b.content_type.as_deref() == Some("html") {
                    html2text::from_read(content.as_bytes(), 80).ok()
                } else {
                    Some(content.to_string())
                }
            })
            .unwrap_or_else(|| "(empty body)".to_string());

        let payload = EmailPayload {
            from,
            subject,
            date,
            body: body_text,
            account_name: format!("{} [Graph]", account.name),
        };

        if let Err(e) =
            send_to_discord(http_client, &account.discord_webhook_url, &payload).await
        {
            error!(
                account = %account.name,
                subject = %payload.subject,
                error = %e,
                "Failed to send to Discord"
            );
            continue;
        }

        // Mark as forwarded locally
        state
            .mark_forwarded(&account.name, "graph", &msg.id)
            .await;

        // Mark as read via Graph API (best-effort)
        if let Err(e) = mark_as_read(http_client, &access_token, &msg.id).await {
            warn!(
                account = %account.name,
                message_id = %msg.id,
                error = %e,
                "Failed to mark message as read via Graph"
            );
        }
    }

    Ok(())
}

async fn mark_as_read(
    http_client: &Client,
    access_token: &str,
    message_id: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let url = format!("{}/{}", GRAPH_MESSAGES_URL, message_id);
    let resp = http_client
        .patch(&url)
        .bearer_auth(access_token)
        .json(&serde_json::json!({"isRead": true}))
        .send()
        .await?;

    if !resp.status().is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("Failed to mark as read: {}", body).into());
    }

    Ok(())
}
