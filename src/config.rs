use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Deserialize, Clone)]
pub struct AppConfig {
    pub accounts: Vec<AccountConfig>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct AccountConfig {
    pub name: String,
    /// Protocol: "imap" (default) or "graph" (Microsoft Graph API).
    #[serde(default = "default_protocol")]
    pub protocol: String,
    /// IMAP server hostname. Required for IMAP protocol.
    #[serde(default)]
    pub imap_server: Option<String>,
    /// IMAP server port. Defaults to 993.
    #[serde(default = "default_imap_port")]
    pub imap_port: u16,
    pub username: String,
    /// Password for basic auth. Not needed if auth_method is "oauth2" or protocol is "graph".
    #[serde(default)]
    pub password: Option<String>,
    /// Authentication method: "password" (default) or "oauth2".
    #[serde(default = "default_auth_method")]
    pub auth_method: String,
    /// Azure AD application (client) ID. Required when auth_method is "oauth2" or protocol is "graph".
    #[serde(default)]
    pub client_id: Option<String>,
    /// OAuth2 scope for token requests.
    #[serde(default = "default_oauth2_scope")]
    pub oauth2_scope: String,
    /// List of mailboxes to monitor. Use ["*"] to monitor all mailboxes.
    /// Only used for IMAP protocol. Defaults to ["INBOX"].
    #[serde(default = "default_mailboxes")]
    pub mailboxes: Vec<String>,
    pub discord_webhook_url: String,
}

fn default_protocol() -> String {
    "imap".to_string()
}

fn default_imap_port() -> u16 {
    993
}

fn default_auth_method() -> String {
    "password".to_string()
}

fn default_oauth2_scope() -> String {
    "https://graph.microsoft.com/Mail.ReadWrite offline_access".to_string()
}

fn default_mailboxes() -> Vec<String> {
    vec!["INBOX".to_string()]
}

impl AppConfig {
    pub fn load(path: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let content = std::fs::read_to_string(path)?;
        let config: AppConfig = serde_json::from_str(&content)?;

        if config.accounts.is_empty() {
            return Err("config must contain at least one account".into());
        }

        for (i, account) in config.accounts.iter().enumerate() {
            if account.username.is_empty() {
                return Err(format!("account[{}]: username is empty", i).into());
            }
            if account.discord_webhook_url.is_empty() {
                return Err(format!("account[{}]: discord_webhook_url is empty", i).into());
            }

            match account.protocol.as_str() {
                "imap" => {
                    if account.imap_server.as_ref().is_none_or(|s| s.is_empty()) {
                        return Err(format!(
                            "account[{}]: imap_server is required for IMAP protocol",
                            i
                        )
                        .into());
                    }
                    if account.auth_method == "password"
                        && account.password.as_ref().is_none_or(|p| p.is_empty())
                    {
                        return Err(format!(
                            "account[{}]: password is required for password auth",
                            i
                        )
                        .into());
                    }
                    if account.auth_method == "oauth2"
                        && account.client_id.as_ref().is_none_or(|c| c.is_empty())
                    {
                        return Err(format!(
                            "account[{}]: client_id is required for oauth2 auth",
                            i
                        )
                        .into());
                    }
                }
                "graph" => {
                    if account.client_id.as_ref().is_none_or(|c| c.is_empty()) {
                        return Err(format!(
                            "account[{}]: client_id is required for Graph protocol",
                            i
                        )
                        .into());
                    }
                }
                other => {
                    return Err(format!(
                        "account[{}]: unknown protocol '{}' (use 'imap' or 'graph')",
                        i, other
                    )
                    .into());
                }
            }
        }

        Ok(config)
    }
}

impl AccountConfig {
    pub fn is_oauth2(&self) -> bool {
        self.auth_method == "oauth2"
    }

    pub fn is_graph(&self) -> bool {
        self.protocol == "graph"
    }

    pub fn imap_server_str(&self) -> &str {
        self.imap_server.as_deref().unwrap_or("")
    }
}
