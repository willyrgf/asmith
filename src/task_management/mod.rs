use chrono::Utc;
use matrix_sdk::ruma::OwnedRoomId;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::{debug, error, info, instrument, warn};

// --- TaskEvent Constants ---
#[derive(Debug, Serialize, Deserialize, Clone)]
pub enum TaskEvent {
    Created,
    StatusUpdated,
    LogAdded,
    TitleEdited,
}

impl TaskEvent {
    pub fn to_string_readable(&self) -> &str {
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
pub struct Task {
    pub id: usize,
    pub title: String,
    pub status: String,
    pub logs: Vec<String>,
    pub internal_logs: Vec<(String, String, String)>, // (timestamp, user, log)
    pub creator: String,
}

impl Task {
    pub fn new(sender: String, id: usize, title: String) -> Self {
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

    pub fn add_internal_log(
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

    pub fn add_log(&mut self, sender: String, log: String) {
        self.logs.push(log.clone());
        let truncated_log = if log.len() > 30 {
            format!("'{}...'", &log[..30])
        } else {
            format!("'{}'", log)
        };
        self.add_internal_log(sender, TaskEvent::LogAdded, Some(truncated_log));
    }

    pub fn set_status(&mut self, sender: String, status: String) {
        let old_status = self.status.clone();
        self.status = status.clone();
        self.add_internal_log(
            sender,
            TaskEvent::StatusUpdated,
            Some(format!("from '{}' to '{}'", old_status, status)),
        );
    }

    pub fn set_title(&mut self, sender: String, title: String) {
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

    pub fn show_details(&self) -> String {
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

    pub fn to_string_short(&self) -> String {
        format!("**[{}] {}**", self.status, self.title)
    }
}

// --- TodoList Struct ---
#[derive(Clone)]
pub struct TodoList {
    message_sender: Arc<dyn crate::messaging::MessageSender>,
    pub storage: Arc<StorageManager>,
}

use crate::messaging::MessageSender;
use crate::storage::StorageManager;
use anyhow::Result;

impl TodoList {
    pub fn new(message_sender: Arc<dyn MessageSender>, storage: Arc<StorageManager>) -> Self {
        Self {
            message_sender,
            storage,
        }
    }

    #[instrument(skip(self), fields(room_id = %room_id))]
    pub async fn add_task(
        &self,
        room_id: &OwnedRoomId,
        sender: String,
        task_title: String,
    ) -> Result<()> {
        debug!(user = %sender, "Starting add task operation");

        // Create a lock on the todo lists and get the current task list for the room (or a new one)
        let mut todo_lists_lock = self.storage.todo_lists.lock().await;
        let room_tasks = todo_lists_lock.entry(room_id.clone()).or_default();

        // Get the next task ID and create a new task
        let next_id = room_tasks.len() + 1;
        let task = Task::new(sender.clone(), next_id, task_title.clone());

        info!(
            user = %sender,
            room_id = %room_id,
            task_id = next_id,
            title = %task_title,
            "Creating new task"
        );

        // Add the task to the room's task list
        room_tasks.push(task);

        // Prepare and send the response message
        let message = format!(
            "📝 Task {} added by {}:\n {}",
            next_id,
            sender,
            room_tasks.last().unwrap().title
        );

        debug!("Sending confirmation message to room");
        self.send_matrix_message(room_id, &message, None).await?;

        debug!("Saving updated task list");
        match self.storage.save().await {
            Ok(_) => {
                info!(
                    user = %sender,
                    room_id = %room_id,
                    task_id = next_id,
                    "Successfully added and saved new task"
                );
            }
            Err(e) => {
                error!(
                    user = %sender,
                    room_id = %room_id,
                    task_id = next_id,
                    error = %e,
                    "Failed to save task list after adding task"
                );
                return Err(e);
            }
        }

        Ok(())
    }

    pub async fn list_tasks(&self, room_id: &OwnedRoomId) -> Result<()> {
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

    #[instrument(skip(self), fields(room_id = %room_id, task_id = task_number))]
    pub async fn done_task(
        &self,
        room_id: &OwnedRoomId,
        sender: String,
        task_number: usize,
    ) -> Result<()> {
        debug!(user = %sender, "Starting mark task as done operation");

        let mut todo_lists = self.storage.todo_lists.lock().await;
        let tasks = todo_lists.entry(room_id.clone()).or_default();

        if task_number > 0 && task_number <= tasks.len() {
            let task = &mut tasks[task_number - 1];
            let task_title = task.title.clone();

            info!(
                user = %sender,
                room_id = %room_id,
                task_id = task_number,
                title = %task_title,
                "Marking task as done"
            );

            task.set_status(sender.clone(), "done".to_string());

            let message = format!("✅ Task {} marked as done: **{}**", task_number, task.title);
            let html_message = format!(
                "✅ Task {} marked as done: <b>{}</b>",
                task_number, task.title
            );

            debug!("Sending confirmation message to room");
            self.send_matrix_message(room_id, &message, Some(html_message))
                .await?;

            debug!("Saving updated task list");
            match self.storage.save().await {
                Ok(_) => {
                    info!(
                        user = %sender,
                        room_id = %room_id,
                        task_id = task_number,
                        "Successfully saved task status change"
                    );
                }
                Err(e) => {
                    error!(
                        user = %sender,
                        room_id = %room_id,
                        task_id = task_number,
                        error = %e,
                        "Failed to save task list after marking task as done"
                    );
                    return Err(e);
                }
            }
        } else {
            warn!(
                user = %sender,
                room_id = %room_id,
                task_id = task_number,
                "Attempted to mark non-existent task as done"
            );

            let message = format!("❌ Error: Task {} doesn't exist.", task_number);
            self.send_matrix_message(room_id, &message, None).await?;
        }

        Ok(())
    }

    pub async fn close_task(
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

    pub async fn log_task(
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
                    "📝 Log Added to Task #{}:<br>Log: '{}'<<br><br><b>Current Task Details:</b><br>{}",
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

    pub async fn details_task(&self, room_id: &OwnedRoomId, task_number: usize) -> Result<()> {
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
                let message = format!("🔍 Task Details:\n{}", details);
                let html_message = format!("🔍 Task Details:<br>{}", details.replace('\n', "<br>"));
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

    // Use MessageSender trait to send messages without directly depending on Matrix SDK
    pub async fn send_matrix_message(
        &self,
        room_id: &OwnedRoomId,
        message: &str,
        html_message: Option<String>,
    ) -> Result<()> {
        self.message_sender
            .send_response(room_id, message, html_message)
            .await
    }

    pub async fn edit_task(
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
