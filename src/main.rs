mod config;
mod discord;
mod graph_listener;
mod imap_listener;
mod oauth2;
mod state;

use config::AppConfig;
use oauth2::OAuth2Manager;
use state::ForwardedState;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::{error, info};

#[tokio::main]
async fn main() {
    // Initialize structured logging
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    // Determine config path: first CLI arg or default "config.json"
    let config_path = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("config.json"));

    info!(path = %config_path.display(), "Loading configuration");

    let app_config = match AppConfig::load(&config_path) {
        Ok(cfg) => cfg,
        Err(e) => {
            error!(error = %e, "Failed to load configuration");
            std::process::exit(1);
        }
    };

    info!(
        accounts = app_config.accounts.len(),
        "Starting email bot"
    );

    // Load local forwarded state
    let state_path = config_path
        .parent()
        .unwrap_or(Path::new("."))
        .join("state.json");
    let state = ForwardedState::load(&state_path);

    // Load OAuth2 token store
    let tokens_path = config_path
        .parent()
        .unwrap_or(Path::new("."))
        .join("tokens.json");
    let oauth2 = Arc::new(OAuth2Manager::load(&tokens_path));

    // Spawn tasks per account
    let mut handles = Vec::new();

    for account in app_config.accounts {
        if account.is_graph() {
            // Microsoft Graph API — single polling task per account
            let account_clone = account.clone();
            let state_clone = state.clone();
            let oauth2_clone = oauth2.clone();
            let handle = tokio::spawn(async move {
                graph_listener::listen(account_clone, state_clone, oauth2_clone).await;
            });
            info!(account = %account.name, protocol = "graph", "Spawned Graph API listener");
            handles.push(handle);
        } else {
            // IMAP — resolve mailboxes and spawn per-mailbox tasks
            let mailboxes = match imap_listener::resolve_mailboxes(&account, &oauth2).await {
                Ok(m) => m,
                Err(e) => {
                    error!(
                        account = %account.name,
                        error = %e,
                        "Failed to resolve mailboxes, skipping account"
                    );
                    continue;
                }
            };

            for mailbox in mailboxes {
                let account_clone = account.clone();
                let mailbox_clone = mailbox.clone();
                let state_clone = state.clone();
                let oauth2_clone = oauth2.clone();
                let handle = tokio::spawn(async move {
                    imap_listener::listen(account_clone, mailbox_clone, state_clone, oauth2_clone)
                        .await;
                });
                info!(account = %account.name, protocol = "imap", mailbox = %mailbox, "Spawned IMAP listener");
                handles.push(handle);
            }
        }
    }

    if handles.is_empty() {
        error!("No listener tasks were spawned. Check your configuration.");
        std::process::exit(1);
    }

    // Wait for Ctrl+C
    tokio::signal::ctrl_c()
        .await
        .expect("Failed to listen for Ctrl+C");

    info!("Received Ctrl+C, shutting down");

    for handle in &handles {
        handle.abort();
    }

    for handle in handles {
        let _ = handle.await;
    }

    info!("Shutdown complete");
}
