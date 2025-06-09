use anyhow::Result;

use once_cell::sync::OnceCell;
use std::sync::Arc;
use tracing::{debug, info};

// Import app constants from config module
use crate::config::{APP_NAME, APP_VERSION};

// Module imports
mod app;
mod bot_commands;
mod config;
mod logging;
mod matrix_integration;
mod messaging;
mod storage;
mod task_management;

// Module components we need to use
use crate::bot_commands::BotCore;
use config::init_config;

// Global access to BotCore
static BOT_CORE: OnceCell<Arc<BotCore>> = OnceCell::new();

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize configuration from arguments and environment variables
    let config = init_config()?;

    // Initialize logging
    logging::init_logging(APP_NAME, config.debug)?;

    info!("Starting {} v{}...", APP_NAME, APP_VERSION);
    debug!("Configuration: {:?}", config);

    // Ensure required directories exist
    app::ensure_directories(&config).await?;

    // Initialize Matrix client, session, and storage manager
    let context = app::init_matrix_client(&config).await?;

    // Setup BotCore and event handlers
    app::setup_bot_core(&context).await?;

    // Auto-load previous bot state if available
    app::auto_load_bot_state(&context.storage_manager).await?;

    // Start the main sync loop
    app::start_sync_loop(&context, &config).await?;

    Ok(())
}
