use crate::config::AccountConfig;
use crate::discord::{send_to_discord, EmailPayload};
use crate::oauth2::{OAuth2Manager, XOAuth2};
use crate::state::ForwardedState;

use async_imap::extensions::idle::IdleResponse;
use futures::StreamExt;
use mail_parser::MessageParser;
use reqwest::Client;
use rustls_pki_types::ServerName;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_util::compat::TokioAsyncReadCompatExt;
use tracing::{error, info, warn};

const MAX_BACKOFF_SECS: u64 = 300; // 5 minutes
const IDLE_TIMEOUT: Duration = Duration::from_secs(29 * 60); // 29 min (RFC recommends < 30)

/// The compat wrapper type for the TLS stream used by async-imap.
type CompatTlsStream =
    tokio_util::compat::Compat<tokio_rustls::client::TlsStream<TcpStream>>;
type ImapSession = async_imap::Session<CompatTlsStream>;

/// Resolve the mailbox list for an account.
/// If mailboxes contains "*", list all mailboxes from the server.
/// Otherwise return the configured list as-is.
pub async fn resolve_mailboxes(
    account: &AccountConfig,
    oauth2: &Arc<OAuth2Manager>,
) -> Result<Vec<String>, Box<dyn std::error::Error + Send + Sync>> {
    let wants_all = account.mailboxes.iter().any(|m| m == "*");

    if !wants_all {
        return Ok(account.mailboxes.clone());
    }

    // Connect to IMAP and LIST all mailboxes
    let mut session = connect(account, oauth2).await?;

    let mut names = Vec::new();
    {
        let mailboxes_stream = session.list(Some(""), Some("*")).await?;
        let mut mailboxes_stream = std::pin::pin!(mailboxes_stream);

        while let Some(result) = mailboxes_stream.next().await {
            match result {
                Ok(mailbox) => {
                    let name = mailbox.name().to_string();
                    // Skip non-selectable mailboxes (e.g. namespace containers)
                    if !mailbox
                        .attributes()
                        .iter()
                        .any(|a| matches!(a, async_imap::types::NameAttribute::NoSelect))
                    {
                        names.push(name);
                    }
                }
                Err(e) => {
                    warn!(account = %account.name, error = %e, "Failed to list mailbox");
                }
            }
        }
    }
    // Stream dropped here, session borrow released

    session.logout().await?;

    info!(
        account = %account.name,
        count = names.len(),
        mailboxes = ?names,
        "Discovered mailboxes"
    );

    Ok(names)
}

/// Main listener loop for a single mailbox within an account.
/// Connects, IDLEs for new mail, fetches & forwards, and reconnects on failure.
pub async fn listen(account: AccountConfig, mailbox: String, state: ForwardedState, oauth2: Arc<OAuth2Manager>) {
    let http_client = Client::new();
    let mut backoff = 1u64;

    loop {
        info!(account = %account.name, mailbox = %mailbox, "Connecting to IMAP server");

        match run_session(&account, &mailbox, &http_client, &state, &oauth2).await {
            Ok(()) => {
                backoff = 1;
            }
            Err(e) => {
                error!(
                    account = %account.name,
                    mailbox = %mailbox,
                    error = %e,
                    backoff_secs = backoff,
                    "IMAP session error, reconnecting"
                );
            }
        }

        tokio::time::sleep(Duration::from_secs(backoff)).await;
        backoff = (backoff * 2).min(MAX_BACKOFF_SECS);
    }
}

async fn connect(
    account: &AccountConfig,
    oauth2: &Arc<OAuth2Manager>,
) -> Result<ImapSession, Box<dyn std::error::Error + Send + Sync>> {
    // Ensure the crypto provider is installed (idempotent)
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    let mut root_store = rustls::RootCertStore::empty();
    root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

    let tls_config = rustls::ClientConfig::builder()
        .with_root_certificates(root_store)
        .with_no_client_auth();
    let connector = TlsConnector::from(Arc::new(tls_config));

    let tcp = TcpStream::connect((account.imap_server_str(), account.imap_port)).await?;
    let server_name = ServerName::try_from(account.imap_server_str().to_string())?;
    let tls_stream = connector.connect(server_name, tcp).await?;
    let compat_stream = tls_stream.compat();

    let client = async_imap::Client::new(compat_stream);

    if account.is_oauth2() {
        // OAuth2 XOAUTH2 authentication
        let client_id = account.client_id.as_deref().unwrap_or("");
        let access_token = oauth2
            .get_access_token(&account.name, client_id, &account.oauth2_scope)
            .await?;

        let auth = XOAuth2 {
            user: account.username.clone(),
            access_token,
        };

        let session = client
            .authenticate("XOAUTH2", auth)
            .await
            .map_err(|e| e.0)?;

        Ok(session)
    } else {
        // Basic password authentication
        let password = account.password.as_deref().unwrap_or("");
        let session = client
            .login(&account.username, password)
            .await
            .map_err(|e| e.0)?;

        Ok(session)
    }
}

async fn run_session(
    account: &AccountConfig,
    mailbox: &str,
    http_client: &Client,
    state: &ForwardedState,
    oauth2: &Arc<OAuth2Manager>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut session = connect(account, oauth2).await?;
    info!(account = %account.name, mailbox = %mailbox, "Connected, selecting mailbox");

    session.select(mailbox).await?;

    // Process any existing unseen messages first
    fetch_and_forward_unseen(&mut session, account, mailbox, http_client, state).await?;

    // IDLE loop — wait for new messages
    loop {
        info!(account = %account.name, mailbox = %mailbox, "Starting IDLE");

        let mut idle = session.idle();
        idle.init().await?;

        let (idle_wait, interrupt) = idle.wait_with_timeout(IDLE_TIMEOUT);
        let idle_result = idle_wait.await?;
        drop(interrupt);

        match idle_result {
            IdleResponse::NewData(_) => {
                info!(account = %account.name, mailbox = %mailbox, "New data received via IDLE");
            }
            IdleResponse::Timeout => {
                info!(account = %account.name, mailbox = %mailbox, "IDLE timeout, re-idling");
            }
            IdleResponse::ManualInterrupt => {
                info!(account = %account.name, mailbox = %mailbox, "IDLE manually interrupted");
            }
        }

        session = idle.done().await?;

        fetch_and_forward_unseen(&mut session, account, mailbox, http_client, state).await?;
    }
}

/// A parsed email ready for forwarding.
struct ParsedEmail {
    uid: Option<u32>,
    payload: EmailPayload,
}

async fn fetch_and_forward_unseen(
    session: &mut ImapSession,
    account: &AccountConfig,
    mailbox: &str,
    http_client: &Client,
    state: &ForwardedState,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Search for UNSEEN messages — respects server-side Seen flag.
    // Local state provides a second layer of dedup for providers that don't persist Seen.
    let uids: Vec<u32> = session.uid_search("UNSEEN").await?.into_iter().collect();

    if uids.is_empty() {
        return Ok(());
    }

    info!(
        account = %account.name,
        mailbox = %mailbox,
        count = uids.len(),
        "Found unseen messages"
    );

    let uid_list: String = uids
        .iter()
        .map(|u| u.to_string())
        .collect::<Vec<_>>()
        .join(",");

    // Collect all parsed emails first so we release the borrow on session
    let parsed_emails = {
        let messages_stream = session
            .uid_fetch(&uid_list, "(UID FLAGS BODY.PEEK[] INTERNALDATE)")
            .await?;

        let parser = MessageParser::default();
        let mut results = Vec::new();

        let mut messages_stream = std::pin::pin!(messages_stream);
        while let Some(fetch_result) = messages_stream.next().await {
            let fetch = match fetch_result {
                Ok(f) => f,
                Err(e) => {
                    warn!(account = %account.name, mailbox = %mailbox, error = %e, "Failed to fetch message");
                    continue;
                }
            };

            let body_bytes = match fetch.body() {
                Some(b) => b,
                None => {
                    warn!(account = %account.name, mailbox = %mailbox, "Message has no body");
                    continue;
                }
            };

            let parsed = match parser.parse(body_bytes) {
                Some(msg) => msg,
                None => {
                    warn!(account = %account.name, mailbox = %mailbox, "Failed to parse email");
                    continue;
                }
            };

            let from = parsed
                .from()
                .and_then(|f| f.first())
                .map(|addr| {
                    if let Some(name) = addr.name() {
                        format!("{} <{}>", name, addr.address().unwrap_or(""))
                    } else {
                        addr.address().unwrap_or("unknown").to_string()
                    }
                })
                .unwrap_or_else(|| "unknown".to_string());

            let subject = parsed.subject().unwrap_or("(no subject)").to_string();

            let date = parsed
                .date()
                .map(|d| d.to_rfc3339())
                .or_else(|| fetch.internal_date().map(|d| d.to_rfc3339()))
                .unwrap_or_else(|| "unknown".to_string());

            let text_body = parsed
                .body_text(0)
                .map(|t| t.to_string())
                .or_else(|| {
                    parsed.body_html(0).and_then(|html| {
                        html2text::from_read(html.as_bytes(), 80).ok()
                    })
                })
                .unwrap_or_else(|| "(empty body)".to_string());

            results.push(ParsedEmail {
                uid: fetch.uid,
                payload: EmailPayload {
                    from,
                    subject,
                    date,
                    body: text_body,
                    account_name: format!("{} [{}]", account.name, mailbox),
                },
            });
        }

        results
    };

    // Now forward each email to Discord and mark as seen
    for email in &parsed_emails {
        if let Some(uid) = email.uid {
            // Skip if already forwarded (per local state)
            if state.is_forwarded(&account.name, mailbox, &uid.to_string()).await {
                continue;
            }
        }

        if let Err(e) =
            send_to_discord(http_client, &account.discord_webhook_url, &email.payload).await
        {
            error!(
                account = %account.name,
                mailbox = %mailbox,
                subject = %email.payload.subject,
                error = %e,
                "Failed to send to Discord"
            );
            // Don't mark as forwarded if Discord send failed
            continue;
        }

        // Mark as forwarded locally
        if let Some(uid) = email.uid {
            state.mark_forwarded(&account.name, mailbox, &uid.to_string()).await;
        }

        // Best-effort: also set IMAP \Seen flag
        if let Some(uid) = email.uid
            && let Err(e) = session.uid_store(uid.to_string(), "+FLAGS (\\Seen)").await
        {
            warn!(
                account = %account.name,
                mailbox = %mailbox,
                uid,
                error = %e,
                "Failed to mark message as seen"
            );
        }
    }

    Ok(())
}
