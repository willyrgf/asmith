use std::env;
use std::path::PathBuf;

// App constants
pub const APP_NAME: &str = env!("CARGO_PKG_NAME");
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

use anyhow::{Result, anyhow};
use clap::Parser;
use matrix_sdk::ruma::{OwnedUserId, UserId};
use tracing::{info, warn};
use url::Url;

// Define the CLI arguments using clap
#[derive(Parser, Debug, Clone)]
#[command(author, version, about)]
pub struct Args {
    /// Directory to store data files (default: platform-specific data directory + /asmith_bot)
    #[clap(long)]
    pub data_dir: Option<PathBuf>,

    /// Matrix homeserver URL (e.g., https://matrix.org)
    #[clap(long)]
    pub homeserver: Option<Url>,

    /// Matrix user ID (e.g., @username:matrix.org)
    #[clap(long)]
    pub user_id: Option<OwnedUserId>,

    /// Matrix user password (can also be set via MATRIX_PASSWORD env variable)
    #[clap(long)]
    pub password: Option<String>,

    /// Matrix access token (can also be set via MATRIX_ACCESS_TOKEN env variable). Overrides password.
    #[clap(long)]
    pub access_token: Option<String>,

    /// Enable debug mode with verbose logging
    #[clap(long)]
    pub debug: bool,

    /// Maximum number of consecutive connection failures before exiting (default: 3)
    #[clap(long, default_value_t = 3)]
    pub max_retries: usize,
}

#[derive(Debug, Clone)]
pub struct BotConfig {
    pub data_dir: PathBuf,
    pub homeserver: Option<Url>,
    pub user_id: Option<OwnedUserId>,
    pub password: Option<String>,
    pub access_token: Option<String>,
    pub debug: bool,
    pub max_retries: usize,
}

impl BotConfig {
    pub fn from_args(args: Args) -> Result<Self> {
        // Get data directory or use platform default
        let data_dir = if let Some(dir) = args.data_dir {
            dir
        } else {
            let mut dir = dirs::data_dir()
                .ok_or_else(|| anyhow!("Failed to determine platform data directory"))?;
            dir.push(APP_NAME);
            dir
        };

        // Create data directory if it doesn't exist
        if !data_dir.exists() {
            std::fs::create_dir_all(&data_dir)?;
            info!("Created data directory at {}", data_dir.display());
        }

        // Check for environment variables for sensitive data
        let password = args.password.or_else(|| env::var("MATRIX_PASSWORD").ok());
        let access_token = args
            .access_token
            .or_else(|| env::var("MATRIX_ACCESS_TOKEN").ok());

        if args.homeserver.is_none() {
            warn!("No homeserver URL specified. Login will not be possible without it.");
        }

        if args.user_id.is_none() {
            warn!("No user ID specified. Login will not be possible without it.");
        }

        if password.is_none() && access_token.is_none() {
            warn!(
                "Neither password nor access token provided. Login will not be possible without one of them."
            );
        }

        Ok(Self {
            data_dir,
            homeserver: args.homeserver,
            user_id: args.user_id,
            password,
            access_token,
            debug: args.debug,
            max_retries: args.max_retries,
        })
    }

    pub fn get_session_file_path(&self) -> PathBuf {
        self.data_dir.join("session.json")
    }


    pub fn get_homeserver(&self) -> Result<&Url> {
        self.homeserver
            .as_ref()
            .ok_or_else(|| anyhow!("Homeserver URL is required but was not provided"))
    }

    pub fn get_user_id(&self) -> Result<&UserId> {
        self.user_id
            .as_ref()
            .map(|id| id.as_ref())
            .ok_or_else(|| anyhow!("User ID is required but was not provided"))
    }

    // Helper method to check if login is possible with current config
    pub fn can_login(&self) -> bool {
        self.homeserver.is_some()
            && self.user_id.is_some()
            && (self.password.is_some() || self.access_token.is_some())
    }
}

// Initialize configuration from command-line arguments and environment variables
pub fn init_config() -> Result<BotConfig> {
    let args = Args::parse();
    BotConfig::from_args(args)
}
