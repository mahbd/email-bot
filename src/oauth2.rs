use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex;
use tracing::{error, info};

const MICROSOFT_DEVICE_CODE_URL: &str =
    "https://login.microsoftonline.com/consumers/oauth2/v2.0/devicecode";
const MICROSOFT_TOKEN_URL: &str =
    "https://login.microsoftonline.com/consumers/oauth2/v2.0/token";


/// Stored OAuth2 tokens for a single account.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenData {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: u64, // Unix timestamp
}

/// All stored tokens, keyed by account name.
#[derive(Debug, Default, Serialize, Deserialize)]
struct TokenStore {
    tokens: std::collections::HashMap<String, TokenData>,
}

/// Manages OAuth2 tokens: device code flow for initial auth, refresh for ongoing use.
pub struct OAuth2Manager {
    path: PathBuf,
    http: Client,
    store: Mutex<TokenStore>,
}

impl OAuth2Manager {
    /// Load token store from disk.
    pub fn load(path: &Path) -> Self {
        let store = if path.exists() {
            match std::fs::read_to_string(path) {
                Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
                Err(_) => TokenStore::default(),
            }
        } else {
            TokenStore::default()
        };

        OAuth2Manager {
            path: path.to_path_buf(),
            http: Client::new(),
            store: Mutex::new(store),
        }
    }

    fn save(&self, store: &TokenStore) {
        if let Ok(content) = serde_json::to_string_pretty(store) {
            let _ = std::fs::write(&self.path, content);
        }
    }

    /// Get a valid access token for the given account.
    /// Refreshes if expired, or initiates device code flow if no token exists.
    pub async fn get_access_token(
        &self,
        account_name: &str,
        client_id: &str,
        scope: &str,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let store = self.store.lock().await;

        if let Some(token_data) = store.tokens.get(account_name) {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs();

            // If token is still valid (with 60s buffer), return it
            if now + 60 < token_data.expires_at {
                return Ok(token_data.access_token.clone());
            }

            // Try to refresh
            info!(account = %account_name, "Access token expired, refreshing");
            let refresh_token = token_data.refresh_token.clone();
            drop(store); // Release lock during HTTP call

            match self.refresh_token(client_id, &refresh_token, scope).await {
                Ok(new_data) => {
                    let access_token = new_data.access_token.clone();
                    let mut store = self.store.lock().await;
                    store.tokens.insert(account_name.to_string(), new_data);
                    self.save(&store);
                    return Ok(access_token);
                }
                Err(e) => {
                    error!(account = %account_name, error = %e, "Token refresh failed, need re-auth");
                    // Fall through to device code flow
                }
            }
        } else {
            drop(store);
        }

        // No token or refresh failed — initiate device code flow
        let token_data = self.device_code_flow(account_name, client_id, scope).await?;
        let access_token = token_data.access_token.clone();

        let mut store = self.store.lock().await;
        store.tokens.insert(account_name.to_string(), token_data);
        self.save(&store);

        Ok(access_token)
    }

    async fn refresh_token(
        &self,
        client_id: &str,
        refresh_token: &str,
        scope: &str,
    ) -> Result<TokenData, Box<dyn std::error::Error + Send + Sync>> {
        let resp = self
            .http
            .post(MICROSOFT_TOKEN_URL)
            .form(&[
                ("client_id", client_id),
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh_token),
                ("scope", scope),
            ])
            .send()
            .await?;

        let status = resp.status();
        let body: serde_json::Value = resp.json().await?;

        if !status.is_success() {
            let err = body["error_description"]
                .as_str()
                .unwrap_or("unknown error");
            return Err(format!("Token refresh failed: {}", err).into());
        }

        parse_token_response(&body)
    }

    async fn device_code_flow(
        &self,
        account_name: &str,
        client_id: &str,
        scope: &str,
    ) -> Result<TokenData, Box<dyn std::error::Error + Send + Sync>> {
        // Step 1: Request device code
        let resp = self
            .http
            .post(MICROSOFT_DEVICE_CODE_URL)
            .form(&[("client_id", client_id), ("scope", scope)])
            .send()
            .await?;

        let status = resp.status();
        let device_resp: serde_json::Value = resp.json().await?;

        if !status.is_success() {
            let err_desc = device_resp["error_description"]
                .as_str()
                .unwrap_or("unknown error");
            let err_code = device_resp["error"].as_str().unwrap_or("unknown");
            return Err(format!(
                "Device code request failed ({}): {} - {}",
                status, err_code, err_desc
            )
            .into());
        }

        let device_code = device_resp["device_code"]
            .as_str()
            .ok_or_else(|| format!("Missing device_code in response: {}", device_resp))?;
        let user_code = device_resp["user_code"]
            .as_str()
            .ok_or("Missing user_code in response")?;
        let verification_uri = device_resp["verification_uri"]
            .as_str()
            .ok_or("Missing verification_uri in response")?;
        let interval = device_resp["interval"].as_u64().unwrap_or(5);
        let expires_in = device_resp["expires_in"].as_u64().unwrap_or(900);

        // Step 2: Display instructions to the user
        println!();
        println!("╔══════════════════════════════════════════════════════════╗");
        println!("║           OAuth2 Authentication Required                ║");
        println!("╠══════════════════════════════════════════════════════════╣");
        println!("║  Account: {:<46} ║", account_name);
        println!("║                                                        ║");
        println!("║  1. Open: {:<46} ║", verification_uri);
        println!("║  2. Enter code: {:<40} ║", user_code);
        println!("║                                                        ║");
        println!("║  Waiting for authorization...                          ║");
        println!("╚══════════════════════════════════════════════════════════╝");
        println!();

        // Step 3: Poll for token
        let deadline = SystemTime::now() + Duration::from_secs(expires_in);
        let poll_interval = Duration::from_secs(interval);

        loop {
            if SystemTime::now() > deadline {
                return Err("Device code flow timed out — user did not authorize in time".into());
            }

            tokio::time::sleep(poll_interval).await;

            let resp = self
                .http
                .post(MICROSOFT_TOKEN_URL)
                .form(&[
                    ("client_id", client_id),
                    ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                    ("device_code", device_code),
                ])
                .send()
                .await?;

            let body: serde_json::Value = resp.json().await?;

            if let Some(error) = body["error"].as_str() {
                match error {
                    "authorization_pending" => continue,
                    "slow_down" => {
                        tokio::time::sleep(Duration::from_secs(5)).await;
                        continue;
                    }
                    _ => {
                        let desc = body["error_description"]
                            .as_str()
                            .unwrap_or("unknown error");
                        return Err(format!("Device code flow failed: {}", desc).into());
                    }
                }
            }

            // Success
            info!(account = %account_name, "OAuth2 authorization successful");
            return parse_token_response(&body);
        }
    }
}

fn parse_token_response(
    body: &serde_json::Value,
) -> Result<TokenData, Box<dyn std::error::Error + Send + Sync>> {
    let access_token = body["access_token"]
        .as_str()
        .ok_or("Missing access_token")?
        .to_string();
    let refresh_token = body["refresh_token"]
        .as_str()
        .ok_or("Missing refresh_token")?
        .to_string();
    let expires_in = body["expires_in"].as_u64().unwrap_or(3600);

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();

    Ok(TokenData {
        access_token,
        refresh_token,
        expires_at: now + expires_in,
    })
}

/// XOAUTH2 authenticator for async-imap.
pub struct XOAuth2 {
    pub user: String,
    pub access_token: String,
}

impl async_imap::Authenticator for XOAuth2 {
    type Response = String;

    fn process(&mut self, _challenge: &[u8]) -> Self::Response {
        format!(
            "user={}\x01auth=Bearer {}\x01\x01",
            self.user, self.access_token
        )
    }
}
