use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{error, info};

/// Key for tracking forwarded emails: "account_name::mailbox"
type MailboxKey = String;

/// Persistent state tracking which email IDs have been forwarded.
/// Stored as a JSON file mapping (account, mailbox) → set of IDs.
/// IDs are strings to support both IMAP UIDs and Graph message IDs.
#[derive(Debug, Clone)]
pub struct ForwardedState {
    path: PathBuf,
    inner: Arc<Mutex<StateData>>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct StateData {
    /// Maps "account_name::mailbox" → set of forwarded IDs
    forwarded: HashMap<MailboxKey, HashSet<String>>,
}

fn make_key(account_name: &str, mailbox: &str) -> MailboxKey {
    format!("{}::{}", account_name, mailbox)
}

impl ForwardedState {
    /// Load state from disk, or create empty state if file doesn't exist.
    pub fn load(path: &Path) -> Self {
        let data = if path.exists() {
            match std::fs::read_to_string(path) {
                Ok(content) => match serde_json::from_str(&content) {
                    Ok(d) => {
                        info!(path = %path.display(), "Loaded forwarded state");
                        d
                    }
                    Err(e) => {
                        error!(error = %e, "Failed to parse state file, starting fresh");
                        StateData::default()
                    }
                },
                Err(e) => {
                    error!(error = %e, "Failed to read state file, starting fresh");
                    StateData::default()
                }
            }
        } else {
            info!(path = %path.display(), "No state file found, starting fresh");
            StateData::default()
        };

        ForwardedState {
            path: path.to_path_buf(),
            inner: Arc::new(Mutex::new(data)),
        }
    }

    /// Check if an ID has already been forwarded for a given account+mailbox.
    pub async fn is_forwarded(&self, account_name: &str, mailbox: &str, id: &str) -> bool {
        let data = self.inner.lock().await;
        let key = make_key(account_name, mailbox);
        data.forwarded
            .get(&key)
            .map(|ids| ids.contains(id))
            .unwrap_or(false)
    }

    /// Mark an ID as forwarded and persist to disk.
    pub async fn mark_forwarded(&self, account_name: &str, mailbox: &str, id: &str) {
        let mut data = self.inner.lock().await;
        let key = make_key(account_name, mailbox);
        data.forwarded
            .entry(key)
            .or_default()
            .insert(id.to_string());

        // Persist to disk
        if let Err(e) = self.save_locked(&data) {
            error!(error = %e, "Failed to save state file");
        }
    }

    fn save_locked(&self, data: &StateData) -> Result<(), Box<dyn std::error::Error>> {
        let content = serde_json::to_string_pretty(data)?;
        std::fs::write(&self.path, content)?;
        Ok(())
    }
}
