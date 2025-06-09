use anyhow::{Context, Result, anyhow};
use matrix_sdk::{Client, config::SyncSettings};
use std::sync::Arc;
use tokio::fs;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use crate::BOT_CORE;
use crate::BotCore;
use crate::config::BotConfig;
use crate::matrix_integration::{self, ClientStoreConfig};
use crate::storage::StorageManager;

pub struct AppContext {
    pub client: Client,
    pub initial_sync_token: Option<String>,
    pub storage_manager: Arc<StorageManager>,
    pub client_store_config: ClientStoreConfig, // Added for session persistence
}

/// Ensures all required application directories exist
pub async fn ensure_directories(config: &BotConfig) -> Result<()> {
    // Ensure data directories exist
    fs::create_dir_all(&config.data_dir).await.context(format!(
        "Failed to create app data directory at {}",
        config.data_dir.display()
    ))?;

    let store_base_path = config.data_dir.join("matrix_sdk_store");
    fs::create_dir_all(&store_base_path).await.context(format!(
        "Failed to create matrix_sdk_store base directory at {}",
        store_base_path.display()
    ))?;

    Ok(())
}

/// Initialize the Matrix client with session persistence
pub async fn init_matrix_client(config: &BotConfig) -> Result<AppContext> {
    if !config.can_login() {
        warn!("Configuration insufficient for login (homeserver, user ID, and credentials required). Proceeding, but login/restore will likely fail.");
        // Optionally, could return Err(anyhow!("Cannot initialize client: Insufficient login credentials"))
        // For now, just warn and let it proceed to fail at login/restore attempt.
    }

    let session_file_path = config.get_session_file_path();
    let store_base_path = config.data_dir.join("matrix_sdk_store");

    // Destructure to get client_store_config as well
    let (client, initial_sync_token, client_store_config) =
        if session_file_path.exists() && config.access_token.is_none() {
            // Try to restore previous session
            match matrix_integration::restore_session(&session_file_path, config).await {
                Ok(session_data) => {
                    info!("Successfully restored Matrix session.");
                    session_data
                }
                Err(e) => {
                    warn!("Failed to restore session ({}). Performing new login.", e);
                    matrix_integration::login_and_save_session(
                        &session_file_path,
                        &store_base_path,
                        config,
                    )
                    .await?
                }
            }
        } else {
            if config.access_token.is_some() {
                info!("Access token provided, forcing new login session.");
            } else {
                info!(
                    "No previous session file found at {}. Performing new login.",
                    session_file_path.display()
                );
            }
            matrix_integration::login_and_save_session(&session_file_path, &store_base_path, config)
                .await?
        };

    info!(
        "Matrix client initialized. User ID: {}",
        client
            .user_id()
            .ok_or_else(|| anyhow!("Client has no user ID after init"))?
    );

    if let Some(token) = &initial_sync_token {
        debug!("Using initial sync token: {}", token);
    }

    // --- Bot's Storage Manager Setup ---
    let app_level_session_id = Uuid::new_v4();
    let storage_manager = Arc::new(
        StorageManager::new(config.data_dir.clone(), app_level_session_id)
            .context("Failed to create bot's StorageManager")?,
    );
    info!(
        "Bot StorageManager initialized. App session ID: {}",
        app_level_session_id
    );

    Ok(AppContext {
        client,
        initial_sync_token,
        storage_manager,
        client_store_config, // Pass the obtained store config
    })
}

/// Setup the BotCore singleton and register event handlers
pub async fn setup_bot_core(context: &AppContext) -> Result<()> {
    // --- Initialize BotCore (singleton) ---
    let bot_core_instance = Arc::new(BotCore::new(
        context.client.clone(),
        context.storage_manager.clone(),
    ));
    BOT_CORE
        .set(bot_core_instance)
        .map_err(|_| anyhow!("Failed to set BOT_CORE singleton"))?;
    info!("BotCore initialized and set globally.");

    // --- Register Event Handlers ---
    context
        .client
        .add_event_handler(matrix_integration::on_stripped_state_member);
    matrix_integration::register_message_handler(&context.client);
    info!("Matrix event handlers registered.");

    // --- Setup Verification Event Handlers ---
    matrix_integration::handle_verification_events(context.client.clone()).await;

    Ok(())
}

/// Load the last saved bot state, if available
pub async fn auto_load_bot_state(storage_manager: &Arc<StorageManager>) -> Result<()> {
    match storage_manager.list_saved_files() {
        Ok(files) => {
            if let Some(most_recent_file) = files.last() {
                info!(
                    "Attempting to auto-load bot state from {}...",
                    most_recent_file
                );
                match storage_manager.load(most_recent_file).await {
                    Ok(true) => info!(
                        "Successfully auto-loaded bot state from {}",
                        most_recent_file
                    ),
                    Ok(false) => warn!(
                        "Failed to auto-load bot state (load returned false) from {}",
                        most_recent_file
                    ),
                    Err(e) => error!(
                        "Error auto-loading bot state from {}: {}",
                        most_recent_file, e
                    ),
                }
            } else {
                info!("No saved bot state files found for auto-loading.");
            }
        }
        Err(e) => error!("Failed to list saved bot state files: {}", e),
    }

    Ok(())
}

/// Start the main sync loop with connection monitoring
pub async fn start_sync_loop(context: &AppContext, config: &BotConfig) -> Result<()> {
    // --- Connection Monitor Setup ---
    let mut connection_monitor = matrix_integration::ConnectionMonitor::new(config.max_retries);
    info!(
        "Connection monitor initialized with max_retries={}",
        config.max_retries
    );
    connection_monitor.connection_successful(); // Mark initial connection as successful

    // --- Sync Loop ---
    let sync_settings = context
        .initial_sync_token
        .as_ref()
        .map(|token| SyncSettings::default().token(token.clone()))
        .unwrap_or_default();

    // Use modularized sync loop function with connection monitor
    let session_file_path = config.get_session_file_path(); // Get session file path

    matrix_integration::start_sync_loop(
        context.client.clone(),
        sync_settings,
        &mut connection_monitor,
        &session_file_path,           // Pass session file path
        &context.client_store_config, // Pass client store config
    )
    .await
}
