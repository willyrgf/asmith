use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use clap::Parser;

use futures_util::stream::StreamExt;
use matrix_sdk::authentication::matrix::MatrixSession;
use matrix_sdk::encryption::verification::Verification;
use matrix_sdk::ruma::events::ToDeviceEvent;
use matrix_sdk::ruma::events::key::verification::VerificationMethod;
use matrix_sdk::{
    Client, Room, RoomState,
    config::SyncSettings,
    ruma::{
        OwnedRoomId, OwnedUserId, RoomId,
        events::room::{
            member::StrippedRoomMemberEvent,
            message::{MessageType, OriginalSyncRoomMessageEvent, RoomMessageEventContent},
        },
    },
};
// Removed: use matrix_sdk_crypto::Sas as CryptoSdkSas; (unused)
use once_cell::sync::OnceCell;
use rand::{Rng, distributions::Alphanumeric, thread_rng};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, path::PathBuf, sync::Arc, time::Duration};
use tokio::fs;
use tokio::{sync::Mutex, time::sleep};
use tracing::{Level, debug, error, info, warn};
use tracing_subscriber::filter::EnvFilter;
use url::Url;
use uuid::Uuid;

static BOT_CORE: OnceCell<Arc<BotCore>> = OnceCell::new();

const APP_NAME: &str = env!("CARGO_PKG_NAME");
const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

// --- Session Persistence Structs ---

#[derive(Debug, Serialize, Deserialize)]
struct ClientConfig {
    homeserver_url: String,
    store_path: PathBuf,
    store_passphrase: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct PersistedSession {
    client_config: ClientConfig,
    matrix_session: MatrixSession,
    sync_token: Option<String>,
}

async fn on_stripped_state_member(
    room_member: StrippedRoomMemberEvent,
    client: Client,
    room: Room,
) {
    if room_member.state_key != client.user_id().unwrap() {
        return;
    }

    tokio::spawn(async move {
        println!("Autojoining room {}", room.room_id());
        let mut delay = 2;

        while let Err(err) = room.join().await {
            // retry autojoin due to synapse sending invites, before the
            // invited user can join for more information see
            // https://github.com/matrix-org/synapse/issues/4345
            eprintln!(
                "Failed to join room {} ({err:?}), retrying in {delay}s",
                room.room_id()
            );

            sleep(Duration::from_secs(delay)).await;
            delay *= 2;

            if delay > 3600 {
                eprintln!("Can't join room {} ({err:?})", room.room_id());
                break;
            }
        }
        println!("Successfully joined room {}", room.room_id());
    });
}

// --- TaskEvent Constants ---
#[derive(Debug, Serialize, Deserialize, Clone)]
enum TaskEvent {
    Created,
    StatusUpdated,
    LogAdded,
    TitleEdited,
}

impl TaskEvent {
    fn to_string_readable(&self) -> &str {
        match self {
            TaskEvent::Created => "Created task",
            TaskEvent::StatusUpdated => "Updated status",
            TaskEvent::LogAdded => "Added log",
            TaskEvent::TitleEdited => "Edited title",
        }
    }
}

// --- Task Struct ---
#[derive(Debug, Serialize, Deserialize, Clone)]
struct Task {
    id: usize,
    title: String,
    status: String,
    logs: Vec<String>,
    internal_logs: Vec<(String, String, String)>, // (timestamp, user, log)
    creator: String,
}

impl Task {
    fn new(sender: String, id: usize, title: String) -> Self {
        let mut task = Task {
            id,
            title,
            status: "pending".to_owned(),
            logs: Vec::new(),
            internal_logs: Vec::new(),
            creator: sender.clone(),
        };
        task.add_internal_log(sender, TaskEvent::Created, None);
        task
    }

    fn add_internal_log(
        &mut self,
        sender: String,
        event_type: TaskEvent,
        extra_info: Option<String>,
    ) {
        let timestamp = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
        let user = sender;
        let action = match extra_info {
            Some(info) => format!("{}: {}", event_type.to_string_readable(), info),
            None => event_type.to_string_readable().to_owned(),
        };
        self.internal_logs.push((timestamp, user, action));
    }

    fn add_log(&mut self, sender: String, log: String) {
        self.logs.push(log.clone());
        let truncated_log = if log.len() > 30 {
            format!("'{}...'", &log[..30])
        } else {
            format!("'{}'", log)
        };
        self.add_internal_log(sender, TaskEvent::LogAdded, Some(truncated_log));
    }

    fn set_status(&mut self, sender: String, status: String) {
        let old_status = self.status.clone();
        self.status = status.clone();
        self.add_internal_log(
            sender,
            TaskEvent::StatusUpdated,
            Some(format!("from '{}' to '{}'", old_status, status)),
        );
    }

    fn set_title(&mut self, sender: String, title: String) {
        let old_title = self.title.clone();
        self.title = title.clone();
        let truncated_old_title = if old_title.len() > 30 {
            format!("'{}...'", &old_title[..30])
        } else {
            format!("'{}'", old_title)
        };
        let truncated_new_title = if title.len() > 30 {
            format!("'{}...'", &title[..30])
        } else {
            format!("'{}'", title)
        };
        self.add_internal_log(
            sender,
            TaskEvent::TitleEdited,
            Some(format!(
                "from {} to {}",
                truncated_old_title, truncated_new_title
            )),
        );
    }

    fn show_details(&self) -> String {
        let mut details = vec![format!("**[{}] {}**", self.status, self.title)];
        details.push(format!("Created by: {}", self.creator));

        if !self.logs.is_empty() {
            details.push("\n**Logs:**".to_owned());
            for (i, log) in self.logs.iter().enumerate() {
                details.push(format!("{}. {}", i + 1, log));
            }
        }

        if !self.internal_logs.is_empty() {
            details.push("\n**History:**".to_owned());
            for (timestamp, user, action) in &self.internal_logs {
                details.push(format!("• {} - {}: {}", timestamp, user, action));
            }
        }
        details.join("\n")
    }

    fn to_string_short(&self) -> String {
        format!("**[{}] {}**", self.status, self.title)
    }
}

// --- StorageManager Struct ---
#[derive(Debug, Serialize, Deserialize, Clone)]
struct StorageData {
    todo_lists: HashMap<OwnedRoomId, Vec<Task>>,
}

struct StorageManager {
    data_dir: PathBuf,
    session_id: Uuid,
    todo_lists: Arc<Mutex<HashMap<OwnedRoomId, Vec<Task>>>>,
    filename_pattern: Regex,
}

impl StorageManager {
    fn new(data_dir: PathBuf, session_id: Uuid) -> Result<Self> {
        if !data_dir.exists() {
            std::fs::create_dir_all(&data_dir)
                .with_context(|| format!("Failed to create data directory: {:?}", data_dir))?;
            info!("Created data directory: {:?}", data_dir);
        }

        let filename_pattern = Regex::new(&format!(
            r"^{}_{}_[0-9]{{4}}-[0-9]{{2}}-[0-9]{{2}}_[0-9]{{2}}-[0-9]{{2}}-[0-9]{{2}}Z\.json$",
            regex::escape(APP_NAME),
            regex::escape(&session_id.to_string())
        ))?;

        Ok(Self {
            data_dir,
            session_id,
            todo_lists: Arc::new(Mutex::new(HashMap::new())),
            filename_pattern,
        })
    }

    async fn save(&self) -> Result<String> {
        let todo_lists = self.todo_lists.lock().await;
        let current_time = Utc::now();
        let filename = format!(
            "{}_{}_{}.json",
            APP_NAME,
            self.session_id,
            current_time.format("%Y-%m-%d_%H-%M-%SZ")
        );
        let filepath = self.data_dir.join(&filename);

        let data = StorageData {
            todo_lists: todo_lists.clone(),
        };
        let json_data = serde_json::to_string_pretty(&data)?;
        tokio::fs::write(&filepath, json_data)
            .await
            .with_context(|| format!("Failed to write to file: {:?}", filepath))?;

        Ok(filename)
    }

    async fn load(&self, filename: &str) -> Result<bool> {
        if !self.filename_pattern.is_match(filename) {
            error!(
                "Attempted to load file with invalid format: {}. Expected format: {}_<session_id>_<YYYY-MM-DD_HH-MM-SS>Z.json",
                filename, APP_NAME
            );
            return Ok(false);
        }

        let filepath = self.data_dir.join(filename);
        if !filepath.exists() {
            warn!("Attempted to load non-existent file: {:?}", filepath);
            return Ok(false);
        }

        let content = tokio::fs::read_to_string(&filepath)
            .await
            .with_context(|| format!("Failed to read file: {:?}", filepath))?;
        let data: StorageData = serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse JSON from file: {:?}", filepath))?;

        let mut todo_lists = self.todo_lists.lock().await;
        *todo_lists = data.todo_lists;
        Ok(true)
    }

    fn list_saved_files(&self) -> Result<Vec<String>> {
        let mut valid_files = Vec::new();
        for entry in std::fs::read_dir(&self.data_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_file() {
                if let Some(filename) = path.file_name().and_then(|s| s.to_str()) {
                    if self.filename_pattern.is_match(filename) {
                        valid_files.push(filename.to_owned());
                    }
                }
            }
        }
        // Sort files based on the timestamp in the filename (YYYY-MM-DD_HH-MM-SS)
        valid_files.sort_by(|a, b| {
            let a_timestamp = a.chars().rev().skip(5).take(19).collect::<String>();
            let b_timestamp = b.chars().rev().skip(5).take(19).collect::<String>();
            a_timestamp.cmp(&b_timestamp)
        });
        Ok(valid_files)
    }
}

// --- Bot Commands Trait ---
#[async_trait]
trait BotCommand {
    async fn send_matrix_message(
        &self,
        room_id: &RoomId,
        message: &str,
        html_message: Option<String>,
    ) -> Result<()>;
}

// --- TodoList Struct ---
#[derive(Clone)] // Add Clone derive
struct TodoList {
    client: Client,
    storage: Arc<StorageManager>,
}

impl TodoList {
    fn new(client: Client, storage: Arc<StorageManager>) -> Self {
        Self { client, storage }
    }

    async fn add_task(
        &self,
        room_id: &OwnedRoomId,
        sender: String,
        task_title: String,
    ) -> Result<()> {
        let mut todo_lists = self.storage.todo_lists.lock().await;
        let tasks = todo_lists.entry(room_id.clone()).or_default();
        let task_id = tasks.len();
        let new_task = Task::new(sender, task_id, task_title.clone());
        tasks.push(new_task.clone());

        let message = format!("✅ Task Added: **{}**", new_task.title);
        let html_message = format!("✅ Task Added: <b>{}</b>", new_task.title);
        self.send_matrix_message(room_id, &message, Some(html_message))
            .await?;
        self.storage.save().await?;
        Ok(())
    }

    async fn list_tasks(&self, room_id: &OwnedRoomId) -> Result<()> {
        let todo_lists = self.storage.todo_lists.lock().await;
        let tasks = todo_lists.get(room_id);

        if let Some(tasks) = tasks {
            if tasks.is_empty() {
                let message = "ℹ️ Info: There are no tasks in this room's to-do list.";
                self.send_matrix_message(room_id, message, None).await?;
                return Ok(());
            }

            let mut response = String::new();
            for (idx, task) in tasks.iter().enumerate() {
                response.push_str(&format!("{}. {}\n", idx + 1, task.to_string_short()));
            }

            let message = format!("📋 Room To-Do List:\n{}", response);
            let html_message = format!("📋 Room To-Do List:<br>{}", response.replace('\n', "<br>"));
            self.send_matrix_message(room_id, &message, Some(html_message))
                .await?;
        } else {
            let message = "ℹ️ Info: There are no tasks in this room's to-do list.";
            self.send_matrix_message(room_id, message, None).await?;
        }
        Ok(())
    }

    async fn done_task(
        &self,
        room_id: &OwnedRoomId,
        sender: String,
        task_number: usize,
    ) -> Result<()> {
        let mut todo_lists = self.storage.todo_lists.lock().await;
        let tasks = todo_lists.get_mut(room_id);

        if let Some(tasks) = tasks {
            if tasks.is_empty() {
                let message = "ℹ️ Info: There are no tasks in this room's to-do list.";
                self.send_matrix_message(room_id, message, None).await?;
                return Ok(());
            }

            if task_number > 0 && task_number <= tasks.len() {
                let mut task = tasks.remove(task_number - 1);
                task.set_status(sender, "done".to_owned());

                let message = format!("✔️ Task Marked as Done: **{}**", task.to_string_short());
                let html_message =
                    format!("✔️ Task Marked as Done: <b>{}</b>", task.to_string_short());
                self.send_matrix_message(room_id, &message, Some(html_message))
                    .await?;
                self.storage.save().await?;
            } else {
                let message = format!(
                    "❌ Error: Invalid task number: {}. Use `!list` to see valid numbers.",
                    task_number
                );
                self.send_matrix_message(room_id, &message, None).await?;
            }
        } else {
            let message = "ℹ️ Info: There are no tasks in this room's to-do list.";
            self.send_matrix_message(room_id, message, None).await?;
        }
        Ok(())
    }

    async fn close_task(
        &self,
        room_id: &OwnedRoomId,
        sender: String,
        task_number: usize,
    ) -> Result<()> {
        let mut todo_lists = self.storage.todo_lists.lock().await;
        let tasks = todo_lists.get_mut(room_id);

        if let Some(tasks) = tasks {
            if tasks.is_empty() {
                let message = "ℹ️ Info: There are no tasks in this room's to-do list.";
                self.send_matrix_message(room_id, message, None).await?;
                return Ok(());
            }

            if task_number > 0 && task_number <= tasks.len() {
                let mut task = tasks.remove(task_number - 1);
                task.set_status(sender, "closed".to_owned());

                let message = format!("✖️ Task Closed: **{}**", task.to_string_short());
                let html_message = format!("✖️ Task Closed: <b>{}</b>", task.to_string_short());
                self.send_matrix_message(room_id, &message, Some(html_message))
                    .await?;
                self.storage.save().await?;
            } else {
                let message = format!(
                    "❌ Error: Invalid task number: {}. Use `!list` to see valid numbers.",
                    task_number
                );
                self.send_matrix_message(room_id, &message, None).await?;
            }
        } else {
            let message = "ℹ️ Info: There are no tasks in this room's to-do list.";
            self.send_matrix_message(room_id, message, None).await?;
        }
        Ok(())
    }

    async fn log_task(
        &self,
        room_id: &OwnedRoomId,
        sender: String,
        task_number: usize,
        log_content: String,
    ) -> Result<()> {
        let mut todo_lists = self.storage.todo_lists.lock().await;
        let tasks = todo_lists.get_mut(room_id);

        if let Some(tasks) = tasks {
            if tasks.is_empty() {
                let message = "ℹ️ Info: There are no tasks in this room's to-do list.";
                self.send_matrix_message(room_id, message, None).await?;
                return Ok(());
            }

            if task_number > 0 && task_number <= tasks.len() {
                let task = &mut tasks[task_number - 1];
                task.add_log(sender, log_content.clone());

                let message = format!(
                    "📝 Log Added to Task #{}:\nLog: '{}'\n\nCurrent Task Details:\n{}",
                    task_number,
                    log_content,
                    task.show_details()
                );
                let html_message = format!(
                    "📝 Log Added to Task #{}:<br>Log: '{}'<br><br><b>Current Task Details:</b><br>{}",
                    task_number,
                    log_content,
                    task.show_details().replace('\n', "<br>")
                );
                self.send_matrix_message(room_id, &message, Some(html_message))
                    .await?;
                self.storage.save().await?;
            } else {
                let message = format!(
                    "❌ Error: Invalid task number: {}. Use `!list` to see valid numbers.",
                    task_number
                );
                self.send_matrix_message(room_id, &message, None).await?;
            }
        } else {
            let message = "ℹ️ Info: There are no tasks in this room's to-do list.";
            self.send_matrix_message(room_id, message, None).await?;
        }
        Ok(())
    }

    async fn details_task(&self, room_id: &OwnedRoomId, task_number: usize) -> Result<()> {
        let todo_lists = self.storage.todo_lists.lock().await;
        let tasks = todo_lists.get(room_id);

        if let Some(tasks) = tasks {
            if tasks.is_empty() {
                let message = "ℹ️ Info: There are no tasks in this room's to-do list.";
                self.send_matrix_message(room_id, message, None).await?;
                return Ok(());
            }

            if task_number > 0 && task_number <= tasks.len() {
                let task = &tasks[task_number - 1];
                let details = task.show_details();

                let message = format!("🔍 Task #{} Details:\n{}", task_number, details);
                let html_message = format!(
                    "🔍 Task #{} Details:<br>{}",
                    task_number,
                    details.replace('\n', "<br>")
                );
                self.send_matrix_message(room_id, &message, Some(html_message))
                    .await?;
            } else {
                let message = format!(
                    "❌ Error: Invalid task number: {}. Use `!list` to see valid numbers.",
                    task_number
                );
                self.send_matrix_message(room_id, &message, None).await?;
            }
        } else {
            let message = "ℹ️ Info: There are no tasks in this room's to-do list.";
            self.send_matrix_message(room_id, message, None).await?;
        }
        Ok(())
    }

    async fn edit_task(
        &self,
        room_id: &OwnedRoomId,
        sender: String,
        task_number: usize,
        new_title: String,
    ) -> Result<()> {
        let mut todo_lists = self.storage.todo_lists.lock().await;
        let tasks = todo_lists.get_mut(room_id);

        if let Some(tasks) = tasks {
            if tasks.is_empty() {
                let message = "ℹ️ Info: There are no tasks in this room's to-do list.";
                self.send_matrix_message(room_id, message, None).await?;
                return Ok(());
            }

            if task_number > 0 && task_number <= tasks.len() {
                let task = &mut tasks[task_number - 1];
                let old_title = task.title.clone();
                task.set_title(sender, new_title.clone());

                let message = format!(
                    "✏️ Task Edited: Task #{} title changed:\nFrom: {}\nTo: {}",
                    task_number, old_title, new_title
                );
                let html_message = format!(
                    "✏️ Task Edited: Task #{} title changed:<br><b>From:</b> {}<br><b>To:</b> {}",
                    task_number, old_title, new_title
                );
                self.send_matrix_message(room_id, &message, Some(html_message))
                    .await?;
                self.storage.save().await?;
            } else {
                let message = format!(
                    "❌ Error: Invalid task number: {}. Use `!list` to see valid numbers.",
                    task_number
                );
                self.send_matrix_message(room_id, &message, None).await?;
            }
        } else {
            let message = "ℹ️ Info: There are no tasks in this room's to-do list.";
            self.send_matrix_message(room_id, message, None).await?;
        }
        Ok(())
    }
}

#[async_trait]
impl BotCommand for TodoList {
    async fn send_matrix_message(
        &self,
        room_id: &RoomId,
        message: &str,
        html_message: Option<String>,
    ) -> Result<()> {
        if let Some(room) = self.client.get_room(room_id) {
            let content_msgtype = if let Some(html_body) = html_message {
                MessageType::text_html(message.to_string(), html_body)
            } else {
                MessageType::text_plain(message.to_string())
            };
            let content = RoomMessageEventContent::new(content_msgtype);
            let send_result = room.send(content).await;
            send_result.with_context(|| format!("Failed to send message to room {}", room_id))?;
        } else {
            return Err(anyhow!(
                "Room with ID {} not found in client's tracked rooms.",
                room_id
            ));
        }
        Ok(())
    }
}

// --- BotManagement Struct ---
#[derive(Clone)] // Add Clone derive
struct BotManagement {
    client: Client,
    storage: Arc<StorageManager>,
}

impl BotManagement {
    fn new(client: Client, storage: Arc<StorageManager>) -> Self {
        Self { client, storage }
    }

    async fn clear_tasks(&self, room_id: &OwnedRoomId) -> Result<()> {
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

    async fn save_command(&self, room_id: &OwnedRoomId) -> Result<()> {
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
                error!("Error during save command: {:?}", e);
                let message = format!(
                    "❌ Error Saving: An error occurred while saving the lists: {}",
                    e
                );
                self.send_matrix_message(room_id, &message, None).await?;
            }
        }
        Ok(())
    }

    async fn load_command(&self, room_id: &OwnedRoomId, filename: String) -> Result<()> {
        if filename.contains("..") || filename.contains('/') {
            let message = "❌ Invalid Filename: Invalid characters detected in filename.";
            self.send_matrix_message(room_id, message, None).await?;
            return Ok(());
        }

        if !self.storage.filename_pattern.is_match(&filename) {
            let message = format!(
                "❌ Invalid Filename Format: Filename '{}' does not match the expected format: `{}_<session_id>_<YYYY-MM-DD_HH-MM-SS>Z.json`",
                filename, APP_NAME
            );
            let html_message = format!(
                "❌ Invalid Filename Format: Filename '<code>{}</code>' does not match the expected format: <code>{}_<session_id>_<YYYY-MM-DD_HH-MM-SS>Z.json</code>",
                filename, APP_NAME
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
                error!("Error loading command: {:?}", e);
                let message = format!(
                    "❌ Error Loading: An error occurred while loading the lists: {}",
                    e
                );
                self.send_matrix_message(room_id, &message, None).await?;
            }
        }
        Ok(())
    }

    async fn loadlast_command(&self, room_id: &OwnedRoomId) -> Result<()> {
        let files = self.storage.list_saved_files()?;

        if files.is_empty() {
            let message = "ℹ️ No Files Found: No saved to-do list files found.";
            self.send_matrix_message(room_id, message, None).await?;
            return Ok(());
        }

        let most_recent_file = files
            .last()
            .context("No files found after sorting")?
            .clone();

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
                error!("Error loading last command: {:?}", e);
                let message = format!(
                    "❌ Error Loading: An error occurred while loading the most recent lists: {}",
                    e
                );
                self.send_matrix_message(room_id, &message, None).await?;
            }
        }
        Ok(())
    }

    async fn list_files_command(&self, room_id: &OwnedRoomId) -> Result<()> {
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
                error!("Error listing files: {:?}", e);
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
        if let Some(room) = self.client.get_room(room_id) {
            let content_msgtype = if let Some(html_body) = html_message {
                MessageType::text_html(message.to_string(), html_body)
            } else {
                MessageType::text_plain(message.to_string())
            };
            let content = RoomMessageEventContent::new(content_msgtype);
            let send_result = room.send(content).await;
            send_result.with_context(|| format!("Failed to send message to room {}", room_id))?;
        } else {
            return Err(anyhow!(
                "Room with ID {} not found in client's tracked rooms.",
                room_id
            ));
        }
        Ok(())
    }
}

// --- New Unified Verification Event Handling ---

async fn handle_verification_events(client: Client) {
    info!("Setting up verification event handlers...");

    // Handler for m.key.verification.request
    client.add_event_handler(
        |ev: ToDeviceEvent<matrix_sdk::ruma::events::key::verification::request::ToDeviceKeyVerificationRequestEventContent>,
         c: Client| async move {
            let sender = ev.sender;
            let flow_id_str = ev.content.transaction_id.to_string(); // Keep original flow_id from event for consistency if needed
            info!(%sender, flow_id = %flow_id_str, "Received m.key.verification.request");

            let encryption_instance = c.encryption(); // Direct assignment, not Option handling
            if let Some(request) = encryption_instance
                .get_verification_request(&sender, &flow_id_str) // Use flow_id_str here
                .await
            {
                info!(%sender, flow_id = %request.flow_id(), "Got SdkVerificationRequest. Accepting with SASv1...");
                if let Err(e) = request.accept_with_methods(vec![VerificationMethod::SasV1]).await {
                    error!(%sender, flow_id = %request.flow_id(), "Failed to accept verification request: {e:?}");
                } else {
                    info!(%sender, flow_id = %request.flow_id(), "Successfully accepted verification request with SASv1.");
                }
            } else {
                warn!(%sender, flow_id = %flow_id_str, "Could not find SdkVerificationRequest after m.key.verification.request, or not SASv1.");
            }
        },
    );
    info!("Registered handler for m.key.verification.request");

    // Handler for m.key.verification.start
    client.add_event_handler(
        |ev: ToDeviceEvent<matrix_sdk::ruma::events::key::verification::start::ToDeviceKeyVerificationStartEventContent>,
         c: Client| async move {
            let sender = ev.sender;
            let flow_id_str = ev.content.transaction_id.to_string(); // Use this flow_id for logging
            info!(%sender, flow_id = %flow_id_str, "Received m.key.verification.start for method {:?} (from_device: {})", ev.content.method, ev.content.from_device);

            let encryption_instance = c.encryption(); // Direct assignment, not Option handling
            if let Some(Verification::SasV1(sas)) = encryption_instance
                .get_verification(&sender, &flow_id_str) // Use flow_id_str here
                .await
            {
                info!(%sender, flow_id = %flow_id_str, "Got SasVerification. Accepting..."); // Use flow_id_str
                if let Err(e) = sas.accept().await {
                    error!(%sender, flow_id = %flow_id_str, "Failed to accept SASv1 verification: {e:?}"); // Use flow_id_str
                } else {
                    info!(%sender, flow_id = %flow_id_str, "Successfully accepted SASv1 verification."); // Use flow_id_str
                }
            } else {
                warn!(%sender, flow_id = %flow_id_str, "Could not find SasVerification after m.key.verification.start, or it's not SASv1.");
            }
        },
    );
    info!("Registered handler for m.key.verification.start");

    // Handler for m.key.verification.key
    client.add_event_handler(
        |ev: ToDeviceEvent<matrix_sdk::ruma::events::key::verification::key::ToDeviceKeyVerificationKeyEventContent>,
         c: Client| async move {
            let sender = ev.sender.clone(); // Clone for potential use in spawned task
            let flow_id_str = ev.content.transaction_id.to_string();
            info!(%sender, flow_id = %flow_id_str, "Received m.key.verification.key");

            let encryption_instance = c.encryption();
            if let Some(Verification::SasV1(sas)) = encryption_instance
                .get_verification(&sender, &flow_id_str)
                .await
            {
                // Clone necessary items for the spawned task
                let sas_clone = sas.clone(); // Sas object from SDK is typically an Arc wrapper, so clone is cheap.
                let _client_clone = c.clone(); 
                let sender_clone = sender.clone();
                let flow_id_clone = flow_id_str.clone();

                tokio::spawn(async move {
                    info!(sender = %sender_clone, flow_id = %flow_id_clone, "Spawned SAS confirmation task.");

                    // The SasVerification struct from matrix_sdk::encryption::sas itself provides these methods.
                    let mut changes_stream = sas_clone.changes();

                    loop {
                        tokio::select! {
                            biased; // Prioritize stream events over timeout if both are ready.

                            // Wait for a change in the SAS state
                            change = changes_stream.next() => {
                                if change.is_none() {
                                    warn!(sender = %sender_clone, flow_id = %flow_id_clone, "SAS changes stream ended before completion or cancellation.");
                                    break; // Stream ended
                                }
                                
                                info!(sender = %sender_clone, flow_id = %flow_id_clone, "SAS state change detected. Re-evaluating.");

                                // Check for cancellation or completion first
                                if sas_clone.is_cancelled() {
                                    info!(sender = %sender_clone, flow_id = %flow_id_clone, "SAS verification was cancelled. Exiting task.");
                                    break;
                                }
    
                                if sas_clone.is_done() {
                                    info!(sender = %sender_clone, flow_id = %flow_id_clone, "SAS verification is done. Exiting task.");
                                    break;
                                }

                                // If not cancelled or done, check if emojis/decimals are available to confirm
                                if sas_clone.emoji().is_some() || sas_clone.decimals().is_some() {
                                    if let Some(emojis) = sas_clone.emoji() {
                                        info!(
                                            sender = %sender_clone,
                                            flow_id = %flow_id_clone,
                                            emojis = ?emojis.iter().map(|e| e.symbol).collect::<Vec<_>>(),
                                            "SAS emojis available. Confirming..."
                                        );
                                    } else if let Some(decimals) = sas_clone.decimals() {
                                        info!(
                                            sender = %sender_clone,
                                            flow_id = %flow_id_clone,
                                            decimals = ?(decimals.0, decimals.1, decimals.2),
                                            "SAS decimals available. Confirming..."
                                        );
                                    }
                                    if let Err(e) = sas_clone.confirm().await { 
                                        error!(sender = %sender_clone, flow_id = %flow_id_clone, "Failed to confirm SASv1 verification: {e:?}");
                                    } else {
                                        info!(sender = %sender_clone, flow_id = %flow_id_clone, "Successfully sent SASv1 confirmation.");
                                    }
                                } else {
                                    debug!(sender = %sender_clone, flow_id = %flow_id_clone, "SAS emojis/decimals still not available after state change. Waiting for next change.");
                                }
                            }
                            // Timeout to prevent task from running indefinitely
                            _ = tokio::time::sleep(Duration::from_secs(90)) => { 
                                warn!(sender = %sender_clone, flow_id = %flow_id_clone, "SAS confirmation task timed out waiting for emojis/decimals or completion.");
                                if !sas_clone.is_done() && !sas_clone.is_cancelled() {
                                   info!(sender = %sender_clone, flow_id = %flow_id_clone, "Attempting to cancel SAS due to timeout.");
                                   if let Err(e) = sas_clone.cancel().await { // Corrected: cancel() takes no arguments
                                        error!(sender = %sender_clone, flow_id = %flow_id_clone, "Failed to cancel SAS verification on timeout: {e:?}");
                                   } else {
                                        info!(sender = %sender_clone, flow_id = %flow_id_clone, "Cancelled SAS verification due to timeout in confirmation task.");
                                   }
                                }
                                break; // Exit task on timeout
                            }
                        }

                        // Explicitly check for completion or cancellation after each select block iteration
                        if sas_clone.is_done() {
                            info!(sender = %sender_clone, flow_id = %flow_id_clone, "SAS verification successfully done after action/event. Exiting task.");
                            break;
                        } 
                        if sas_clone.is_cancelled() { // Check separately in case it was cancelled by our timeout action
                            info!(sender = %sender_clone, flow_id = %flow_id_clone, "SAS verification cancelled after action/event. Exiting task.");
                            break;
                        }
                    }
                    info!(sender = %sender_clone, flow_id = %flow_id_clone, "SAS confirmation task finished.");
                });
            } else {
                warn!(%sender, flow_id = %flow_id_str, "Could not find SasVerification after m.key.verification.key, or it's not SASv1. Cannot start confirmation task.");
            }
        },
    );
    info!("Registered handler for m.key.verification.key");

    // Handler for m.key.verification.mac
    client.add_event_handler(
        |ev: ToDeviceEvent<matrix_sdk::ruma::events::key::verification::mac::ToDeviceKeyVerificationMacEventContent>,
         _c: Client| async move {
            let sender = ev.sender;
            let flow_id = ev.content.transaction_id.to_string();
            info!(%sender, %flow_id, "Received m.key.verification.mac. Keys: {:?}, MAC: {:?}", ev.content.keys, ev.content.mac);
            // Typically, the SDK handles this internally. We're just logging.
        },
    );
    info!("Registered handler for m.key.verification.mac");

    // Handler for m.key.verification.cancel
    client.add_event_handler(
        |ev: ToDeviceEvent<matrix_sdk::ruma::events::key::verification::cancel::ToDeviceKeyVerificationCancelEventContent>,
         _c: Client| async move {
            let sender = ev.sender;
            let flow_id = ev.content.transaction_id.to_string();
            info!(%sender, %flow_id, "Received m.key.verification.cancel. Code: {}, Reason: {}", ev.content.code, ev.content.reason);
        },
    );
    info!("Registered handler for m.key.verification.cancel");

    // Handler for m.key.verification.done
    client.add_event_handler(
        |ev: ToDeviceEvent<matrix_sdk::ruma::events::key::verification::done::ToDeviceKeyVerificationDoneEventContent>,
         _c: Client| async move {
            let sender = ev.sender;
            let flow_id = ev.content.transaction_id.to_string();
            info!(%sender, %flow_id, "Received m.key.verification.done");
        },
    );
    info!("Registered handler for m.key.verification.done");

    info!("All verification event handlers registered.");
}

// --- End of New Unified Verification Event Handling ---

// --- BotCore Struct ---
#[derive(Clone)]
struct BotCore {
    todo_list: Arc<TodoList>,
    bot_management: Arc<BotManagement>,
    // storage field removed as it's managed by submodules
}

impl BotCore {
    fn new(client: Client, storage_manager: Arc<StorageManager>) -> Self {
        Self {
            todo_list: Arc::new(TodoList::new(client.clone(), storage_manager.clone())),
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
#[allow(dead_code)]
struct ConnectionMonitor {
    max_retries: usize,
    consecutive_failures: usize,
    total_failures: usize,
    failure_types: HashMap<String, usize>,
    last_failure_time: Option<DateTime<Utc>>,
    first_failure_time: Option<DateTime<Utc>>,
}

impl ConnectionMonitor {
    fn new(max_retries: usize) -> Self {
        Self {
            max_retries,
            consecutive_failures: 0,
            total_failures: 0,
            failure_types: HashMap::new(),
            last_failure_time: None,
            first_failure_time: None,
        }
    }

    fn connection_successful(&mut self) {
        if self.consecutive_failures > 0 {
            info!(
                "Connection restored after {} consecutive failures",
                self.consecutive_failures
            );
        }
        self.consecutive_failures = 0;
    }

    #[allow(dead_code)]
    fn connection_failed(&mut self, error_type: String) -> bool {
        let now = Utc::now();
        if self.consecutive_failures == 0 {
            self.first_failure_time = Some(now);
        }

        self.consecutive_failures += 1;
        self.total_failures += 1;
        self.last_failure_time = Some(now);

        *self.failure_types.entry(error_type.clone()).or_insert(0) += 1;

        let elapsed = self.first_failure_time.map(|first| {
            let elapsed_seconds = (now - first).num_seconds() as f64;
            format!("{:.1} seconds", elapsed_seconds)
        });

        warn!(
            "Connection failure #{}: {}. Total failures: {}{}",
            self.consecutive_failures,
            error_type,
            self.total_failures,
            if let Some(e) = elapsed {
                format!(" in {}", e)
            } else {
                "".to_owned()
            }
        );

        let critical_errors = [
            "LoginError",
            "SyncError",
            "LocalProtocolError",
            "EncryptionError",
        ];
        if critical_errors.contains(&error_type.as_str()) && self.consecutive_failures >= 2 {
            error!(
                "Critical connection error: {}. Exiting immediately.",
                error_type
            );
            return true;
        }

        if self.consecutive_failures >= self.max_retries {
            error!(
                "Maximum connection retries ({}) reached. Failure types: {:?}",
                self.max_retries, self.failure_types
            );
            return true;
        }

        false
    }

    #[allow(dead_code)]
    fn get_status_report(&self) -> String {
        if self.total_failures == 0 {
            return "No connection failures detected".to_owned();
        }

        let mut status = vec![
            "Connection Status Report:".to_owned(),
            format!("- Total failures: {}", self.total_failures),
            format!("- Consecutive failures: {}", self.consecutive_failures),
        ];

        if let Some(first) = self.first_failure_time {
            status.push(format!(
                "- First failure: {}",
                first.format("%Y-%m-%d %H:%M:%S")
            ));
        }

        if let Some(last) = self.last_failure_time {
            status.push(format!(
                "- Latest failure: {}",
                last.format("%Y-%m-%d %H:%M:%S")
            ));
        }

        if let (Some(first), Some(last)) = (self.first_failure_time, self.last_failure_time) {
            let elapsed_seconds = (last - first).num_seconds() as f64;
            status.push(format!(
                "- Problem duration: {:.1} seconds",
                elapsed_seconds
            ));
        }

        status.push("- Failure types:".to_owned());
        if self.failure_types.is_empty() {
            status.push("  - None recorded".to_owned());
        } else {
            let mut sorted_failures: Vec<(&String, &usize)> = self.failure_types.iter().collect();
            sorted_failures.sort_by(|a, b| b.1.cmp(a.1));
            for (error_type, count) in sorted_failures {
                let percentage = (*count as f64 / self.total_failures as f64) * 100.0;
                status.push(format!(
                    "  - {}: {} ({:.1}%)",
                    error_type, count, percentage
                ));
            }
        }
        status.join("\n")
    }
}

// --- Command Line Arguments ---
#[derive(Parser, Debug)]
#[clap(author, version = APP_VERSION, about = format!("{} - A Matrix To-Do List Bot", APP_NAME))]
struct Args {
    /// Directory to store data files (default: platform-specific data directory + /asmith_bot)
    #[clap(long)]
    data_dir: Option<PathBuf>,

    /// Matrix homeserver URL (e.g., https://matrix.org)
    #[clap(long)]
    homeserver: Option<Url>,

    /// Matrix user ID (e.g., @username:matrix.org)
    #[clap(long)]
    user_id: Option<OwnedUserId>,

    /// Matrix user password (can also be set via MATRIX_PASSWORD env variable)
    #[clap(long)]
    password: Option<String>,

    /// Matrix access token (can also be set via MATRIX_ACCESS_TOKEN env variable). Overrides password.
    #[clap(long)]
    access_token: Option<String>,

    /// Enable debug mode with verbose logging
    #[clap(long)]
    debug: bool,

    /// Maximum number of consecutive connection failures before exiting (default: 3)
    #[clap(long, default_value_t = 3)]
    max_retries: usize,
}

// --- Main Function ---

async fn restore_session(
    session_file_path: &PathBuf,
    _args: &Args, // _args might be used later for specific restore logic if needed
) -> Result<(Client, Option<String>)> {
    info!(
        "Attempting to restore session from: {}",
        session_file_path.display()
    );
    let session_json = fs::read_to_string(session_file_path)
        .await
        .context(format!(
            "Failed to read session file: {}",
            session_file_path.display()
        ))?;
    let persisted_session: PersistedSession =
        serde_json::from_str(&session_json).context("Failed to deserialize session data")?;

    let client_config = persisted_session.client_config;
    let matrix_session = persisted_session.matrix_session;

    info!(
        "Restoring client with homeserver: {}",
        client_config.homeserver_url
    );
    info!("Using store path: {}", client_config.store_path.display());

    let client = Client::builder()
        .homeserver_url(client_config.homeserver_url)
        .sqlite_store(
            &client_config.store_path,
            Some(&client_config.store_passphrase),
        )
        .build()
        .await
        .context("Failed to build client during session restore")?;

    client
        .restore_session(matrix_session.clone())
        .await
        .context("Failed to restore Matrix session")?;
    info!(
        "Successfully restored session for user: {}",
        matrix_session.meta.user_id
    );

    Ok((client, persisted_session.sync_token))
}

async fn login_and_save_session(
    session_file_path: &PathBuf,
    store_base_path: &PathBuf,
    args: &Args,
) -> Result<(Client, Option<String>)> {
    info!("Performing new login.");

    let homeserver_url = args
        .homeserver
        .clone()
        .ok_or_else(|| anyhow!("Homeserver URL not provided"))?;
    let user_id = args
        .user_id
        .clone()
        .ok_or_else(|| anyhow!("User ID not provided"))?;

    // Create a unique directory for this session's store
    let mut rng = thread_rng();
    let store_subdir_name: String = std::iter::repeat_with(|| rng.sample(Alphanumeric))
        .map(char::from)
        .take(12)
        .collect();
    let store_path = store_base_path.join(store_subdir_name);
    fs::create_dir_all(&store_path).await.context(format!(
        "Failed to create store directory at {}",
        store_path.display()
    ))?;

    let store_passphrase: String = std::iter::repeat_with(|| rng.sample(Alphanumeric))
        .map(char::from)
        .take(32)
        .collect();

    info!(
        "Building client for new login. Homeserver: {}",
        homeserver_url
    );
    info!("New SQLite store will be at: {}", store_path.display());

    let client = Client::builder()
        .homeserver_url(homeserver_url.as_str())
        .sqlite_store(&store_path, Some(&store_passphrase))
        .build()
        .await
        .context("Failed to build client for new login")?;

    if let Some(access_token) = &args.access_token {
        info!("Logging in with access token.");
        client
            .matrix_auth()
            .login_token(access_token.as_str())
            .await
            .context("Failed to login with access token")?;
    } else if let Some(password) = &args
        .password
        .clone()
        .or_else(|| std::env::var("MATRIX_PASSWORD").ok())
    {
        info!("Logging in with username/password for user: {}", user_id);
        client
            .matrix_auth()
            .login_username(user_id.as_str(), password.as_str())
            .initial_device_display_name(APP_NAME)
            .send()
            .await
            .context(format!(
                "Failed to login with username/password for user {}",
                user_id
            ))?;
    } else {
        return Err(anyhow!(
            "No password or access token provided for Matrix login."
        ));
    }

    info!(
        "Login successful for user: {}",
        client
            .user_id()
            .ok_or_else(|| anyhow!("User ID not available after login"))?
    );

    let matrix_session = client
        .matrix_auth()
        .session()
        .ok_or_else(|| anyhow!("Failed to get session after login"))?;
    let client_config = ClientConfig {
        homeserver_url: homeserver_url.to_string(),
        store_path,
        store_passphrase,
    };

    let persisted_session = PersistedSession {
        client_config,
        matrix_session,
        sync_token: None, // Sync token is not needed for a fresh login session persistence
    };

    let session_json = serde_json::to_string_pretty(&persisted_session)
        .context("Failed to serialize session data")?;
    fs::write(session_file_path, session_json)
        .await
        .context(format!(
            "Failed to write session file to {}",
            session_file_path.display()
        ))?;
    info!("Session saved to: {}", session_file_path.display());

    Ok((client, None)) // No initial_sync_token needed here as client.sync_token() was used for persisted_session.sync_token
}

// --- Obsolete Verification Event Handlers ---
// The functions handle_verification_request and handle_sas_verification were previously defined here.
// They have been removed as their functionality is now consolidated into the
// handle_verification_events function, which uses the latest matrix-sdk event handling mechanisms.

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    // ... (rest of the code remains the same)
    // Setup logging based on args.debug or default
    let default_log_level = if args.debug {
        Level::DEBUG
    } else {
        Level::INFO
    };
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        EnvFilter::new(format!("{},matrix_sdk={}", APP_NAME, default_log_level))
    });

    tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_target(true)
        .init();

    info!("Starting {} v{}...", APP_NAME, APP_VERSION);
    debug!("Parsed arguments: {:?}", args);

    // --- Application Data Directory Setup ---
    let app_data_dir = match args.data_dir.clone() {
        Some(path) => path,
        None => ::dirs::data_dir()
            .ok_or_else(|| {
                anyhow!("Could not determine a default data directory for the application.")
            })?
            .join(APP_NAME),
    };
    fs::create_dir_all(&app_data_dir).await.context(format!(
        "Failed to create app data directory at {}",
        app_data_dir.display()
    ))?;
    info!(
        "Using application data directory: {}",
        app_data_dir.display()
    );

    let session_file_path = app_data_dir.join("matrix_session.json");
    let store_base_path = app_data_dir.join("matrix_sdk_store");
    fs::create_dir_all(&store_base_path).await.context(format!(
        "Failed to create matrix_sdk_store base directory at {}",
        store_base_path.display()
    ))?;

    // --- Matrix Client Setup (with session persistence) ---
    let (client, initial_sync_token) = if session_file_path.exists() && !args.access_token.is_some()
    {
        // Don't restore if access_token is given, force new login
        match restore_session(&session_file_path, &args).await {
            Ok(session_data) => {
                info!("Successfully restored session.");
                session_data
            }
            Err(e) => {
                warn!("Failed to restore session ({}). Performing new login.", e);
                login_and_save_session(&session_file_path, &store_base_path, &args).await?
            }
        }
    } else {
        if args.access_token.is_some() {
            info!("Access token provided, forcing new login session.");
        } else {
            info!(
                "No previous session file found at {}. Performing new login.",
                session_file_path.display()
            );
        }
        login_and_save_session(&session_file_path, &store_base_path, &args).await?
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

    // --- Bot's Own Storage Manager Setup (e.g., for To-Do lists) ---
    // This session_id is for the bot's application-level data, distinct from Matrix session.
    let app_level_session_id = Uuid::new_v4();
    let storage_manager = Arc::new(
        StorageManager::new(app_data_dir.clone(), app_level_session_id)
            .context("Failed to create bot's StorageManager")?,
    );
    info!(
        "Bot StorageManager initialized. App session ID: {}",
        app_level_session_id
    );

    // --- Initialize BotCore (singleton) ---
    let bot_core_instance = Arc::new(BotCore::new(client.clone(), storage_manager.clone()));
    BOT_CORE
        .set(bot_core_instance)
        .map_err(|_| anyhow!("Failed to set BOT_CORE singleton"))?;
    info!("BotCore initialized and set globally.");

    // --- Register Event Handlers ---
    client.add_event_handler(on_stripped_state_member);
    client.add_event_handler(
        // Closure for room messages
        move |ev: OriginalSyncRoomMessageEvent, room: Room, _client_clone: Client| async move {
            if room.state() != RoomState::Joined {
                return;
            }
            // Avoid processing our own messages if desired, though BotCore might handle this
            // if ev.sender == client_clone.user_id().unwrap() { return; }

            let bot_core_ref = BOT_CORE.get().expect("BOT_CORE not initialized").clone();
            tokio::spawn(async move {
                let room_id_owned = room.room_id().to_owned();
                let sender = ev.sender.to_string();

                if let matrix_sdk::ruma::events::room::message::MessageType::Text(text_content) =
                    ev.content.msgtype
                {
                    let body = text_content.body;
                    if body.starts_with('!') {
                        debug!(
                            "Received command: {} from {} in room {}",
                            body, sender, room_id_owned
                        );
                        // Remove the leading '!' before splitting command and args
                        let command_and_args = body.strip_prefix('!').unwrap_or_default().trim();
                        let mut command_parts = command_and_args.splitn(2, ' ');
                        let command = command_parts.next().unwrap_or("").to_lowercase();
                        let args_str = command_parts.next().unwrap_or("").to_owned();

                        if !command.is_empty() {
                            if let Err(e) = bot_core_ref
                                .process_command(
                                    room_id_owned.as_str(),
                                    sender.clone(),
                                    &command,
                                    args_str,
                                )
                                .await
                            {
                                error!(
                                    "Error processing command '{}' from sender {}: {:?}",
                                    command, sender, e
                                );
                                // BotCore::process_command is responsible for sending user-facing error messages.
                                // We just log the error here at the event handler level.
                            }
                        } else {
                            debug!(
                                "Empty command received (only '!') from sender {} in room {}",
                                sender, room_id_owned
                            );
                        }
                    }
                } else {
                    // Not a text message, or not a command. For now, we only process text commands.
                    // You might want to log or handle other message types if needed.
                    // debug!("Received non-command or non-text message from {} in room {}", sender, room_id_owned);
                }
            });
        },
    );
    info!("Matrix event handlers registered.");

    // --- Connection Monitor Setup ---
    let mut connection_monitor = ConnectionMonitor::new(args.max_retries);
    info!(
        "Connection monitor initialized with max_retries={}",
        args.max_retries
    );
    connection_monitor.connection_successful(); // Call after client init

    // --- Setup Verification Event Handlers ---
    handle_verification_events(client.clone()).await;

    // --- Auto-load last saved state for bot's data ---
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

    // --- Sync Loop ---
    let sync_settings = initial_sync_token
        .map(|token| SyncSettings::default().token(token.clone()))
        .unwrap_or_else(SyncSettings::default);

    info!("Starting Matrix sync loop...");
    // The sync method is blocking. It will run until an error occurs or the program is stopped.
    if let Err(e) = client.sync(sync_settings).await {
        error!("Sync loop exited with error: {}", e);
        connection_monitor.connection_failed(format!("Sync loop error: {}", e));
        // Depending on ConnectionMonitor logic, this might lead to exit or retries if implemented.
        return Err(e.into()); // Propagate the error to exit main
    } else {
        // This part might not be reached if sync errors out and returns Err.
        // If sync completes without error (e.g. cancelled by another part of SDK), log it.
        info!("Sync loop finished gracefully (this is unusual for a long-running bot).");
    }

    Ok(())
}
