use anyhow::{Result, anyhow};
use matrix_sdk::{Client, ruma::OwnedRoomId};
use once_cell::sync::OnceCell;
use std::sync::Arc;
use tracing::{debug, error, info};

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
use bot_commands::BotManagement;
use config::init_config;
use storage::StorageManager;
use task_management::TodoList;

// Global access to BotCore
static BOT_CORE: OnceCell<Arc<BotCore>> = OnceCell::new();

// --- BotManagement Struct ---

// Verification event handling moved to matrix_integration/mod.rs

// --- BotCore Struct ---
#[derive(Clone)]
struct BotCore {
    todo_list: Arc<TodoList>,
    bot_management: Arc<BotManagement>,
    // storage field removed as it's managed by submodules
}

impl BotCore {
    fn new(client: Client, storage_manager: Arc<StorageManager>) -> Self {
        // Create the message sender implementation
        let message_sender = Arc::new(crate::messaging::MatrixMessageSender::new(client.clone()));

        Self {
            todo_list: Arc::new(TodoList::new(
                message_sender.clone(),
                storage_manager.clone(),
            )),
            bot_management: Arc::new(BotManagement::new(client.clone(), storage_manager.clone())),
            // storage field removed
        }
    }

    async fn process_command(
        &self,
        room_id_str: &str, // Changed to &str
        sender: String,
        command: &str,
        args_str: String,
    ) -> Result<()> {
        let room_id_owned = match OwnedRoomId::try_from(room_id_str) {
            Ok(id) => id,
            Err(e) => {
                error!("Failed to parse room_id_str '{}': {}", room_id_str, e);
                return Err(anyhow!("Invalid room ID format '{}': {}", room_id_str, e));
            }
        };

        match command {
            "add" | "a" => {
                self.todo_list
                    .add_task(&room_id_owned, sender, args_str)
                    .await
            }
            "list" | "ls" | "l" => self.todo_list.list_tasks(&room_id_owned).await,
            "done" | "d" => {
                if let Ok(task_number) = args_str.trim().parse::<usize>() {
                    self.todo_list
                        .done_task(&room_id_owned, sender, task_number)
                        .await
                } else {
                    self.todo_list
                        .send_matrix_message(
                            &room_id_owned,
                            &format!(
                                "❌ Error: Invalid task number for `!{}`. Use `!help` for usage.",
                                command
                            ),
                            None,
                        )
                        .await
                }
            }
            "close" | "c" => {
                if let Ok(task_number) = args_str.trim().parse::<usize>() {
                    self.todo_list
                        .close_task(&room_id_owned, sender, task_number)
                        .await
                } else {
                    self.todo_list
                        .send_matrix_message(
                            &room_id_owned,
                            &format!(
                                "❌ Error: Invalid task number for `!{}`. Use `!help` for usage.",
                                command
                            ),
                            None,
                        )
                        .await
                }
            }
            "log" | "lg" => {
                let mut log_parts = args_str.splitn(2, ' ');
                if let (Some(task_num_str), Some(log_content)) =
                    (log_parts.next(), log_parts.next())
                {
                    if let Ok(task_number) = task_num_str.trim().parse::<usize>() {
                        self.todo_list
                            .log_task(&room_id_owned, sender, task_number, log_content.to_owned())
                            .await
                    } else {
                        self.todo_list.send_matrix_message(
                                        &room_id_owned,
                            &format!("❌ Error: Invalid task number for `!{}`. Use `!help` for usage.", command),
                            None,
                        ).await
                    }
                } else {
                    self.todo_list.send_matrix_message(
                                &room_id_owned,
                        &format!("❌ Error: Usage: `!log <task_number> <note>`. Use `!help` for usage."),
                        None,
                    ).await
                }
            }
            "details" | "det" => {
                if let Ok(task_number) = args_str.trim().parse::<usize>() {
                    self.todo_list
                        .details_task(&room_id_owned, task_number)
                        .await
                } else {
                    self.todo_list.send_matrix_message(
                                &room_id_owned,
                        &format!("❌ Error: Invalid task number for `!details`. Use `!help` for usage."),
                        None,
                    ).await
                }
            }
            "edit" | "e" => {
                let mut edit_parts = args_str.splitn(2, ' ');
                if let (Some(task_num_str), Some(new_title_content)) =
                    (edit_parts.next(), edit_parts.next())
                {
                    if let Ok(task_number) = task_num_str.trim().parse::<usize>() {
                        self.todo_list
                            .edit_task(
                                &room_id_owned,
                                sender,
                                task_number,
                                new_title_content.to_owned(),
                            )
                            .await
                    } else {
                        self.todo_list.send_matrix_message(
                                        &room_id_owned,
                            &format!("❌ Error: Invalid task number for `!{}`. Use `!help` for usage.", command),
                            None,
                        ).await
                    }
                } else {
                    self.todo_list.send_matrix_message(
                                &room_id_owned,
                        &format!("❌ Error: Usage: `!edit <task_number> <new_title>`. Use `!help` for usage."),
                        None,
                    ).await
                }
            }
            "clear" | "clr" => self.bot_management.clear_tasks(&room_id_owned).await,
            "save" | "s" => self.bot_management.save_command(&room_id_owned).await,
            "load" | "ld" => {
                self.bot_management
                    .load_command(&room_id_owned, args_str)
                    .await
            }
            "loadlast" | "ll" => self.bot_management.loadlast_command(&room_id_owned).await,
            "list_files" | "lf" => self.bot_management.list_files_command(&room_id_owned).await,
            "help" | "h" => {
                let help_message = format!(
                    "**{} Bot Commands:**\n\
                    `!add <task>`: Add a new task.\n\
                    `!list`: List all tasks.\n\
                    `!done <task_number>`: Mark a task as done and remove it.\n\
                    `!close <task_number>`: Close a task without completing and remove it.\n\
                    `!log <task_number> <note>`: Add a note to a task.\n\
                    `!details <task_number>`: Show details of a task.\n\
                    `!edit <task_number> <new_title>`: Edit a task's title.\n\
                    `!clear`: Clear all tasks in the current room.\n\
                    `!save`: Manually save the current state.\n\
                    `!load <filename>`: Load state from a file.\n\
                    `!loadlast`: Load the most recently saved state.\n\
                    `!list_files`: List all saved files.\n\
                    `!help`: Show this help message.",
                    APP_NAME
                );
                let html_help_message = format!(
                    "<b>{} Bot Commands:</b><br>\
                    <code>!add <task></code>: Add a new task.<br>\
                    <code>!list</code>: List all tasks.<br>\
                    <code>!done <task_number></code>: Mark a task as done and remove it.<br>\
                    <code>!close <task_number></code>: Close a task without completing and remove it.<br>\
                    <code>!log <task_number> <note></code>: Add a note to a task.<br>\
                    <code>!details <task_number></code>: Show details of a task.<br>\
                    <code>!edit <task_number> <new_title></code>: Edit a task's title.<br>\
                    <code>!clear</code>: Clear all tasks in the current room.<br>\
                    <code>!save</code>: Manually save the current state.<br>\
                    <code>!load <filename></code>: Load state from a file.<br>\
                    <code>!loadlast</code>: Load the most recently saved state.<br>\
                    <code>!list_files</code>: List all saved files.<br>\
                    <code>!help</code>: Show this help message.",
                    APP_NAME
                );
                self.todo_list
                    .send_matrix_message(&room_id_owned, &help_message, Some(html_help_message))
                    .await
            }
            _ => {
                self.todo_list
                    .send_matrix_message(
                        &room_id_owned,
                        &format!(
                            "Unknown command: `{}`. Type `!help` for a list of commands.",
                            command
                        ),
                        None,
                    )
                    .await
            }
        }
    }
}

// --- ConnectionMonitor Struct ---
// ConnectionMonitor moved to matrix_integration module

// --- Main Function ---

// --- Obsolete Verification Event Handlers ---
// The functions handle_verification_request and handle_sas_verification were previously defined here.
// They have been removed as their functionality is now consolidated into the
// handle_verification_events function, which uses the latest matrix-sdk event handling mechanisms.

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
