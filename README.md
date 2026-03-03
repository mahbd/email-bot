# Email Bot: Email to Discord Forwarder

A Rust application that monitors multiple email accounts and forwards new messages to Discord channels via webhooks. Supports **IMAP** (Gmail, custom servers) and **Microsoft Graph API** (Outlook/Hotmail).

## Features

- **IMAP with IDLE** — push-based, real-time email notifications (Gmail, custom servers)
- **Microsoft Graph API** — REST polling for Outlook/Hotmail accounts
- **Multi-mailbox** — monitor all mailboxes or specific ones per account
- **OAuth2 Device Code Flow** — for Microsoft accounts (no browser needed on server)
- **Local state tracking** — prevents duplicate forwarding across restarts
- **Exponential backoff** — automatic reconnection on failures
- **Discord rate limit handling** — retries with respect to Discord's rate limits
- **Graceful shutdown** — clean Ctrl+C handling

## Setup

### Prerequisites

- [Rust](https://rustup.rs/) 1.85+ (edition 2024)
- A Discord webhook URL
- Email account credentials (IMAP password or OAuth2)

### Discord Webhook

1. Open your Discord server
2. Go to **Channel Settings → Integrations → Webhooks**
3. Click **New Webhook**, copy the URL

### Gmail (IMAP)

Gmail requires an [App Password](https://support.google.com/accounts/answer/185833):

1. Enable 2-Factor Authentication on your Google account
2. Go to [App Passwords](https://myaccount.google.com/apppasswords)
3. Generate a password for "Mail"

Any IMAP server should work with this app.

### Outlook / Hotmail (Graph API)

Microsoft personal accounts require OAuth2 via Graph API:

1. Go to [Azure Portal → App registrations](https://portal.azure.com/#view/Microsoft_AAD_RegisteredApps/ApplicationsListBlade)
2. Click **New registration**
   - Name: `Email Bot`
   - Supported account types: **Personal Microsoft accounts only**
   - Redirect URI: leave blank
3. Copy the **Application (client) ID**
4. Go to **API permissions → Add a permission → Microsoft Graph → Delegated**
   - Add `Mail.ReadWrite`
   - Click **Grant admin consent**
5. Go to **Authentication → Advanced settings**
   - Set **Allow public client flows** to **Yes**
   - Click **Save**

## Configuration

```bash
cp config.example.json config.json
```

### Gmail (IMAP) Account

```json
{
  "name": "Personal Gmail",
  "protocol": "imap",
  "imap_server": "imap.gmail.com",
  "imap_port": 993,
  "username": "you@gmail.com",
  "password": "your-app-password",
  "mailboxes": ["*"],
  "discord_webhook_url": "https://discord.com/api/webhooks/..."
}
```

### Outlook / Hotmail (Graph) Account

```json
{
  "name": "Personal Hotmail",
  "protocol": "graph",
  "auth_method": "oauth2",
  "client_id": "YOUR_AZURE_CLIENT_ID",
  "username": "you@hotmail.com",
  "discord_webhook_url": "https://discord.com/api/webhooks/..."
}
```

### Configuration Fields

| Field | Required | Default | Description |
|-------|----------|---------|-------------|
| `name` | ✅ | — | Display name for the account |
| `protocol` | — | `"imap"` | `"imap"` or `"graph"` |
| `imap_server` | IMAP only | — | IMAP server hostname |
| `imap_port` | — | `993` | IMAP server port |
| `username` | ✅ | — | Email address |
| `password` | IMAP password auth | — | IMAP password or app password |
| `auth_method` | — | `"password"` | `"password"` or `"oauth2"` |
| `client_id` | OAuth2/Graph | — | Azure AD Application (client) ID |
| `oauth2_scope` | — | `Mail.ReadWrite` | OAuth2 scope (override if needed) |
| `mailboxes` | — | `["INBOX"]` | Mailboxes to monitor. `["*"]` = all |
| `discord_webhook_url` | ✅ | — | Discord webhook URL |

## Running

```bash
# Build and run
cargo run

# Custom config path
cargo run -- /path/to/config.json

# Debug logging
RUST_LOG=debug cargo run
```

On first run with an OAuth2 account, you'll see a device code prompt:

```
╔══════════════════════════════════════════════════════════╗
║           OAuth2 Authentication Required                ║
║  1. Open: https://www.microsoft.com/link                 ║
║  2. Enter code: ABCD1234                                 ║
║  Waiting for authorization...                            ║
╚══════════════════════════════════════════════════════════╝
```

Open the URL in any browser, enter the code, and authorize. Tokens are saved to `tokens.json` and refreshed automatically.

## Production Deployment with systemd

### 1. Build Release Binary

```bash
cargo build --release
```

### 2. Set Up Directory

```bash
sudo mkdir -p /opt/email-bot
sudo cp target/release/email-bot /opt/email-bot/
sudo cp config.json /opt/email-bot/
```

### 3. Create System User

```bash
sudo useradd --system --no-create-home --shell /usr/sbin/nologin email-bot
sudo chown -R email-bot:email-bot /opt/email-bot
sudo chmod 600 /opt/email-bot/config.json
```

### 4. Create systemd Service

```bash
sudo tee /etc/systemd/system/email-bot.service << 'EOF'
[Unit]
Description=Email to Discord Forwarder
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=email-bot
Group=email-bot
WorkingDirectory=/opt/email-bot
ExecStart=/opt/email-bot/email-bot
Restart=always
RestartSec=10

# Logging
Environment=RUST_LOG=info
StandardOutput=journal
StandardError=journal
SyslogIdentifier=email-bot

# Security hardening
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=/opt/email-bot
PrivateTmp=true
ProtectKernelTunables=true
ProtectKernelModules=true
ProtectControlGroups=true

[Install]
WantedBy=multi-user.target
EOF
```

### 5. Enable and Start

```bash
sudo systemctl daemon-reload
sudo systemctl enable email-bot
sudo systemctl start email-bot
```

### 6. Manage the Service

```bash
# Check status
sudo systemctl status email-bot

# View logs
sudo journalctl -u email-bot -f

# Restart after config changes
sudo systemctl restart email-bot

# Stop
sudo systemctl stop email-bot
```

> **Note:** If using OAuth2 (Graph), you must run `email-bot` interactively the first time to complete the device code authorization. After `tokens.json` is created, the service can run unattended with automatic token refresh.

## Files

| File | Description | Git-tracked |
|------|-------------|:-----------:|
| `config.json` | Account credentials & settings | ❌ |
| `config.example.json` | Template configuration | ✅ |
| `state.json` | Forwarded message IDs (auto-created) | ❌ |
| `tokens.json` | OAuth2 tokens (auto-created) | ❌ |

## Security

> ⚠️ **`config.json`, `state.json`, and `tokens.json` contain secrets and are gitignored.**
>
> Never commit them to version control. Use `config.example.json` as a template.

## License

MIT
