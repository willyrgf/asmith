use crate::storage::StorageManager;
use crate::task_management::TodoList;
use anyhow::Result;
use async_trait::async_trait;
use matrix_sdk::{
    Client,
    ruma::{OwnedRoomId, RoomId},
};
use std::sync::Arc;

#[async_trait]
pub trait BotCommand: Send + Sync {
    async fn send_matrix_message(
        &self,
        room_id: &RoomId,
        message: &str,
        html_message: Option<String>,
    ) -> Result<()>;
}

#[derive(Clone)]
pub struct BotManagement {
    message_sender: Arc<dyn crate::messaging::MessageSender>,
    pub storage: Arc<StorageManager>,
}

impl BotManagement {
    pub fn new(client: Client, storage: Arc<StorageManager>) -> Self {
        // Create a message sender for this instance
        let message_sender = Arc::new(crate::messaging::MatrixMessageSender::new(client));
        Self {
            message_sender,
            storage,
        }
    }

    pub async fn clear_tasks(&self, room_id: &OwnedRoomId) -> Result<()> {
        let mut todo_lists = self.storage.todo_lists.lock().await;
        if todo_lists.contains_key(room_id) && !todo_lists[room_id].is_empty() {
            todo_lists.insert(room_id.clone(), Vec::new());
            let message = "🗑️ List Cleared: The room's to-do list has been cleared.";
            self.send_matrix_message(room_id, message, None).await?;
            self.storage.save().await?;
        } else {
            let message = "ℹ️ Info: There are no tasks in this room's to-do list to clear.";
            self.send_matrix_message(room_id, message, None).await?;
        }
        Ok(())
    }

    pub async fn save_command(&self, room_id: &OwnedRoomId) -> Result<()> {
        match self.storage.save().await {
            Ok(filename) => {
                let message = format!(
                    "💾 Lists Saved: The to-do lists have been saved to `{}`.",
                    filename
                );
                let html_message = format!(
                    "💾 Lists Saved: The to-do lists have been saved to <code>{}</code>.",
                    filename
                );
                self.send_matrix_message(room_id, &message, Some(html_message))
                    .await?;
            }
            Err(e) => {
                let message = format!(
                    "❌ Error Saving: An error occurred while saving the lists: {}",
                    e
                );
                self.send_matrix_message(room_id, &message, None).await?;
            }
        }
        Ok(())
    }

    pub async fn load_command(&self, room_id: &OwnedRoomId, filename: String) -> Result<()> {
        if filename.contains("..") || filename.contains('/') {
            let message = "❌ Invalid Filename: Invalid characters detected in filename.";
            self.send_matrix_message(room_id, message, None).await?;
            return Ok(());
        }

        if !self.storage.filename_pattern.is_match(&filename) {
            let message = format!(
                "❌ Invalid Filename Format: Filename '{}' does not match the expected format.",
                filename
            );
            let html_message = format!(
                "❌ Invalid Filename Format: Filename '<code>{}</code>' does not match the expected format.",
                filename
            );
            self.send_matrix_message(room_id, &message, Some(html_message))
                .await?;
            return Ok(());
        }

        match self.storage.load(&filename).await {
            Ok(true) => {
                let message = format!(
                    "📂 Lists Loaded: Successfully loaded to-do lists from `{}`.",
                    filename
                );
                let html_message = format!(
                    "📂 Lists Loaded: Successfully loaded to-do lists from <code>{}</code>.",
                    filename
                );
                self.send_matrix_message(room_id, &message, Some(html_message))
                    .await?;
            }
            Ok(false) => {
                let message = format!(
                    "❌ Error Loading: Failed to load lists from `{}`. Check the filename and ensure it's a valid save file.",
                    filename
                );
                let html_message = format!(
                    "❌ Error Loading: Failed to load lists from <code>{}</code>. Check the filename and ensure it's a valid save file.",
                    filename
                );
                self.send_matrix_message(room_id, &message, Some(html_message))
                    .await?;
            }
            Err(e) => {
                let message = format!(
                    "❌ Error Loading: An error occurred while loading the lists: {}",
                    e
                );
                self.send_matrix_message(room_id, &message, None).await?;
            }
        }
        Ok(())
    }

    pub async fn loadlast_command(&self, room_id: &OwnedRoomId) -> Result<()> {
        let files = self.storage.list_saved_files()?;

        if files.is_empty() {
            let message = "ℹ️ No Files Found: No saved to-do list files found.";
            self.send_matrix_message(room_id, message, None).await?;
            return Ok(());
        }

        let most_recent_file = files.last().cloned().unwrap();

        match self.storage.load(&most_recent_file).await {
            Ok(true) => {
                let message = format!(
                    "📂 Last List Loaded: Successfully loaded the most recent lists from `{}`.",
                    most_recent_file
                );
                let html_message = format!(
                    "📂 Last List Loaded: Successfully loaded the most recent lists from <code>{}</code>.",
                    most_recent_file
                );
                self.send_matrix_message(room_id, &message, Some(html_message))
                    .await?;
            }
            Ok(false) => {
                let message = format!(
                    "❌ Error Loading: Failed to load the most recent lists from `{}`. The file might be corrupted.",
                    most_recent_file
                );
                let html_message = format!(
                    "❌ Error Loading: Failed to load the most recent lists from <code>{}</code>. The file might be corrupted.",
                    most_recent_file
                );
                self.send_matrix_message(room_id, &message, Some(html_message))
                    .await?;
            }
            Err(e) => {
                let message = format!(
                    "❌ Error Loading: An error occurred while loading the most recent lists: {}",
                    e
                );
                self.send_matrix_message(room_id, &message, None).await?;
            }
        }
        Ok(())
    }

    pub async fn list_files_command(&self, room_id: &OwnedRoomId) -> Result<()> {
        match self.storage.list_saved_files() {
            Ok(files) => {
                if files.is_empty() {
                    let message = "ℹ️ No Files Found: No saved to-do list files found.";
                    self.send_matrix_message(room_id, message, None).await?;
                } else {
                    let files_list = files
                        .iter()
                        .enumerate()
                        .map(|(i, f)| format!("{}. `{}`", i + 1, f))
                        .collect::<Vec<String>>()
                        .join("\n");
                    let html_files_list = files
                        .iter()
                        .enumerate()
                        .map(|(i, f)| format!("{}. <code>{}</code>", i + 1, f))
                        .collect::<Vec<String>>()
                        .join("<br>");
                    let message = format!("📄 Available Save Files:\n{}", files_list);
                    let html_message = format!("📄 Available Save Files:<br>{}", html_files_list);
                    self.send_matrix_message(room_id, &message, Some(html_message))
                        .await?;
                }
            }
            Err(e) => {
                let message = format!(
                    "❌ Error Listing Files: An error occurred while listing saved files: {}",
                    e
                );
                self.send_matrix_message(room_id, &message, None).await?;
            }
        }
        Ok(())
    }
}

#[async_trait]
impl BotCommand for BotManagement {
    async fn send_matrix_message(
        &self,
        room_id: &RoomId,
        message: &str,
        html_message: Option<String>,
    ) -> Result<()> {
        // Convert RoomId to OwnedRoomId for compatibility with MessageSender trait
        let owned_room_id = room_id.to_owned();
        // Use the MessageSender trait to send the message
        self.message_sender
            .send_response(&owned_room_id, message, html_message)
            .await
    }
}
// --- BotCore Struct ---
#[derive(Clone)]
pub struct BotCore {
    pub todo_lists: Arc<TodoList>,
    pub bot_management: Arc<BotManagement>,
}

impl BotCore {
    pub fn new(client: Client, storage_manager: Arc<StorageManager>) -> Self {
        // Create the message sender for all components
        let message_sender = Arc::new(crate::messaging::MatrixMessageSender::new(client.clone()));

        // Initialize with the message sender
        let todo_lists = Arc::new(TodoList::new(
            message_sender.clone(),
            storage_manager.clone(),
        ));
        let bot_management = Arc::new(BotManagement::new(client.clone(), storage_manager));

        Self {
            todo_lists,
            bot_management,
        }
    }

    pub async fn process_command(
        &self,
        room_id_str: &str,
        sender: String,
        command: &str,
        args_str: String,
    ) -> Result<()> {
        let room_id = room_id_str.parse::<OwnedRoomId>()?;

        match command.trim().to_lowercase().as_str() {
            // Task management commands
            "add" => {
                self.todo_lists
                    .add_task(&room_id, sender.clone(), args_str.clone())
                    .await?
            }
            "list" => self.todo_lists.list_tasks(&room_id).await?,
            "done" => {
                if let Some(id) = parse_task_id(args_str.trim()) {
                    self.todo_lists
                        .done_task(&room_id, sender.clone(), id)
                        .await?;
                } else {
                    let message = "⚠️ Error: Invalid task ID. Please provide a valid task number.";
                    self.todo_lists
                        .send_matrix_message(&room_id, message, None)
                        .await?
                }
            }
            "close" => {
                if let Some(id) = parse_task_id(args_str.trim()) {
                    self.todo_lists
                        .close_task(&room_id, sender.clone(), id)
                        .await?;
                } else {
                    let message = "⚠️ Error: Invalid task ID. Please provide a valid task number.";
                    self.todo_lists
                        .send_matrix_message(&room_id, message, None)
                        .await?
                }
            }
            "log" => {
                let args = args_str.trim();
                if args.is_empty() {
                    let message = "⚠️ Error: Missing task ID and log message.";
                    self.todo_lists
                        .send_matrix_message(&room_id, message, None)
                        .await?
                } else if let Some((id_str, log_msg)) = args.split_once(char::is_whitespace) {
                    if let Some(id) = parse_task_id(id_str) {
                        self.todo_lists
                            .log_task(&room_id, sender.clone(), id, log_msg.trim().to_string())
                            .await?;
                    } else {
                        let message =
                            "⚠️ Error: Invalid task ID. Please provide a valid task number.";
                        self.todo_lists
                            .send_matrix_message(&room_id, message, None)
                            .await?
                    }
                } else if let Some(id) = parse_task_id(args) {
                    // Just the ID, but no log message - show the task details with logs
                    self.todo_lists.details_task(&room_id, id).await?;
                } else {
                    let message = "⚠️ Error: Unable to parse task ID and log message. Format: !log 1 Your log message";
                    self.todo_lists
                        .send_matrix_message(&room_id, message, None)
                        .await?
                }
            }
            "details" => {
                if let Some(id) = parse_task_id(args_str.trim()) {
                    self.todo_lists.details_task(&room_id, id).await?;
                } else {
                    let message = "⚠️ Error: Invalid task ID. Please provide a valid task number.";
                    self.todo_lists
                        .send_matrix_message(&room_id, message, None)
                        .await?
                }
            }
            "edit" => {
                let args = args_str.trim();
                if args.is_empty() {
                    let message = "⚠️ Error: Missing task ID and new description.";
                    self.todo_lists
                        .send_matrix_message(&room_id, message, None)
                        .await?
                } else if let Some((id_str, new_description)) = args.split_once(char::is_whitespace)
                {
                    if let Some(id) = parse_task_id(id_str) {
                        self.todo_lists
                            .edit_task(
                                &room_id,
                                sender.clone(),
                                id,
                                new_description.trim().to_string(),
                            )
                            .await?
                    } else {
                        let message =
                            "⚠️ Error: Invalid task ID. Please provide a valid task number.";
                        self.todo_lists
                            .send_matrix_message(&room_id, message, None)
                            .await?
                    }
                } else {
                    let message = "⚠️ Error: Unable to parse task ID and new description. Format: !edit 1 New task description";
                    self.todo_lists
                        .send_matrix_message(&room_id, message, None)
                        .await?
                }
            }

            // Bot management commands
            "bot" => {
                let args = args_str.trim().to_lowercase();
                let args_parts: Vec<&str> = args.split_whitespace().collect();
                let bot_command = args_parts.first().cloned().unwrap_or("");

                match bot_command {
                    "save" => self.bot_management.save_command(&room_id).await?,
                    "load" => {
                        if args_parts.len() < 2 {
                            let message = "⚠️ Error: Missing filename. Usage: !bot load <filename>";
                            self.bot_management
                                .send_matrix_message(&room_id, message, None)
                                .await?;
                        } else {
                            let filename = args_parts[1].to_string();
                            self.bot_management.load_command(&room_id, filename).await?
                        }
                    }
                    "loadlast" => self.bot_management.loadlast_command(&room_id).await?,
                    "listfiles" => self.bot_management.list_files_command(&room_id).await?,
                    "cleartasks" => self.bot_management.clear_tasks(&room_id).await?,
                    _ => {
                        let usage = "Bot Commands Usage:\n\n\
                        !bot save - Save all lists\n\
                        !bot load <filename> - Load lists from file\n\
                        !bot loadlast - Load most recent save file\n\
                        !bot listfiles - List all save files\n\
                        !bot cleartasks - Clear the current room's list";

                        self.bot_management
                            .send_matrix_message(&room_id, usage, None)
                            .await?;
                    }
                }
            }

            // Help command
            "help" => {
                let help_text = "Matrix ToDo Bot Help:\n\n\
                **Task Commands:**\n\
                !add <task description> - Add a new task\n\
                !list - List all tasks\n\
                !done <id> - Mark a task as done\n\
                !close <id> - Mark a task as closed/completed\n\
                !log <id> <message> - Add a log entry to a task\n\
                !log <id> - Show logs for a task\n\
                !details <id> - Show full task details\n\
                !edit <id> <new description> - Edit a task description\n\n\
                **Bot Commands:**\n\
                !bot save - Save all lists\n\
                !bot load <filename> - Load lists from file\n\
                !bot loadlast - Load most recent save file\n\
                !bot listfiles - List all save files\n\
                !bot cleartasks - Clear the current room's list\n\n\
                **Other Commands:**\n\
                !help - Show this help message";

                let html_help = "<h4>Matrix ToDo Bot Help</h4>\
                <strong>Task Commands:</strong><br>\
                <code>!add &lt;task description&gt;</code> - Add a new task<br>\
                <code>!list</code> - List all tasks<br>\
                <code>!done &lt;id&gt;</code> - Mark a task as done<br>\
                <code>!close &lt;id&gt;</code> - Mark a task as closed/completed<br>\
                <code>!log &lt;id&gt; &lt;message&gt;</code> - Add a log entry to a task<br>\
                <code>!log &lt;id&gt;</code> - Show logs for a task<br>\
                <code>!details &lt;id&gt;</code> - Show full task details<br>\
                <code>!edit &lt;id&gt; &lt;new description&gt;</code> - Edit a task description<br><br>\
                <strong>Bot Commands:</strong><br>\
                <code>!bot save</code> - Save all lists<br>\
                <code>!bot load &lt;filename&gt;</code> - Load lists from file<br>\
                <code>!bot loadlast</code> - Load most recent save file<br>\
                <code>!bot listfiles</code> - List all save files<br>\
                <code>!bot cleartasks</code> - Clear the current room's list<br><br>\
                <strong>Other Commands:</strong><br>\
                <code>!help</code> - Show this help message";

                self.todo_lists
                    .send_matrix_message(&room_id, help_text, Some(html_help.to_string()))
                    .await?;
            }

            // Unknown command
            _ => {
                let message = format!(
                    "⚠️ Unknown command: '{}'. Type !help for available commands.",
                    command
                );
                self.todo_lists
                    .send_matrix_message(&room_id, &message, None)
                    .await?;
            }
        }
        Ok(())
    }
}

// Helper function to parse task IDs
fn parse_task_id(id_str: &str) -> Option<usize> {
    id_str.parse::<usize>().ok()
}
