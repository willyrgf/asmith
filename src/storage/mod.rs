use anyhow::{Context, Result};
use chrono::Utc;
use matrix_sdk::ruma::OwnedRoomId;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, path::PathBuf, sync::Arc};
use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use crate::task_management::Task;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct StorageData {
    pub todo_lists: HashMap<OwnedRoomId, Vec<Task>>,
}

#[derive(Debug, Clone)]
pub struct StorageManager {
    pub data_dir: PathBuf,
    pub session_id: Uuid,
    pub todo_lists: Arc<Mutex<HashMap<OwnedRoomId, Vec<Task>>>>,
    pub filename_pattern: Regex,
}

impl StorageManager {
    pub fn new(data_dir: PathBuf, session_id: Uuid) -> Result<Self> {
        if !data_dir.exists() {
            std::fs::create_dir_all(&data_dir)
                .with_context(|| format!("Failed to create data directory: {:?}", data_dir))?;
        }
        let filename_pattern = Regex::new(&format!(
            r"^{}_{}_[0-9]{{4}}-[0-9]{{2}}-[0-9]{{2}}_[0-9]{{2}}-[0-9]{{2}}-[0-9]{{2}}Z\\.json$",
            regex::escape(env!("CARGO_PKG_NAME")),
            regex::escape(&session_id.to_string())
        ))?;
        Ok(Self {
            data_dir,
            session_id,
            todo_lists: Arc::new(Mutex::new(HashMap::new())),
            filename_pattern,
        })
    }

    pub async fn save(&self) -> Result<String> {
        debug!(session_id = %self.session_id, "Starting task storage save operation");

        let todo_lists = self.todo_lists.lock().await;
        let current_time = Utc::now();
        let filename = format!(
            "{}_{}_{}.json",
            env!("CARGO_PKG_NAME"),
            self.session_id,
            current_time.format("%Y-%m-%d_%H-%M-%SZ")
        );
        let filepath = self.data_dir.join(&filename);

        let task_count = todo_lists
            .iter()
            .fold(0, |acc, (_, tasks)| acc + tasks.len());
        let room_count = todo_lists.len();

        info!(
            session_id = %self.session_id,
            file_path = %filepath.display(),
            task_count,
            room_count,
            "Saving todo lists to file"
        );

        let data = StorageData {
            todo_lists: todo_lists.clone(),
        };

        let json_data = match serde_json::to_string_pretty(&data) {
            Ok(json) => json,
            Err(e) => {
                error!(
                    session_id = %self.session_id,
                    error = %e,
                    "Failed to serialize task data to JSON"
                );
                return Err(e.into());
            }
        };

        match tokio::fs::write(&filepath, json_data).await {
            Ok(_) => {
                info!(
                    session_id = %self.session_id,
                    file_name = %filename,
                    file_path = %filepath.display(),
                    task_count,
                    room_count,
                    "Successfully saved todo lists to file"
                );
                Ok(filename)
            }
            Err(e) => {
                error!(
                    session_id = %self.session_id,
                    file_path = %filepath.display(),
                    error = %e,
                    "Failed to write task data to file"
                );
                Err(anyhow::anyhow!(
                    "Failed to write to file: {:?} - {}",
                    filepath,
                    e
                ))
            }
        }
    }

    pub async fn load(&self, filename: &str) -> Result<bool> {
        debug!(session_id = %self.session_id, filename, "Starting task storage load operation");

        let filepath = self.data_dir.join(filename);
        if !filepath.exists() {
            warn!(session_id = %self.session_id, file_path = %filepath.display(), "Attempted to load non-existent file");
            return Ok(false);
        }

        if !self.filename_pattern.is_match(filename) {
            warn!(
                session_id = %self.session_id,
                filename,
                "Rejected loading file with invalid filename pattern"
            );
            return Ok(false);
        }

        info!(session_id = %self.session_id, file_path = %filepath.display(), "Loading task data from file");

        let file_content = match tokio::fs::read_to_string(&filepath).await {
            Ok(content) => content,
            Err(e) => {
                error!(
                    session_id = %self.session_id,
                    file_path = %filepath.display(),
                    error = %e,
                    "Failed to read task data file"
                );
                return Err(e.into());
            }
        };

        let data: StorageData = match serde_json::from_str(&file_content) {
            Ok(parsed) => parsed,
            Err(e) => {
                error!(
                    session_id = %self.session_id,
                    file_path = %filepath.display(),
                    error = %e,
                    "Failed to parse task data from JSON"
                );
                return Err(e.into());
            }
        };

        let mut todo_lists = self.todo_lists.lock().await;
        *todo_lists = data.todo_lists;

        let task_count = todo_lists
            .iter()
            .fold(0, |acc, (_, tasks)| acc + tasks.len());
        let room_count = todo_lists.len();

        info!(
            session_id = %self.session_id,
            file_path = %filepath.display(),
            task_count,
            room_count,
            "Successfully loaded todo lists from file"
        );

        Ok(true)
    }

    pub fn list_saved_files(&self) -> Result<Vec<String>> {
        debug!(session_id = %self.session_id, data_dir = %self.data_dir.display(), "Listing saved task files");

        let mut valid_files = Vec::new();

        let read_dir_result = match std::fs::read_dir(&self.data_dir) {
            Ok(entries) => entries,
            Err(e) => {
                error!(
                    session_id = %self.session_id,
                    data_dir = %self.data_dir.display(),
                    error = %e,
                    "Failed to read data directory"
                );
                return Err(e.into());
            }
        };

        for entry_result in read_dir_result {
            let entry = match entry_result {
                Ok(e) => e,
                Err(e) => {
                    warn!(
                        session_id = %self.session_id,
                        error = %e,
                        "Failed to read directory entry"
                    );
                    continue;
                }
            };

            let path = entry.path();
            if path.is_file() {
                if let Some(filename) = path.file_name().and_then(|s| s.to_str()) {
                    if self.filename_pattern.is_match(filename) {
                        debug!(file_name = %filename, "Found valid task file");
                        valid_files.push(filename.to_owned());
                    } else {
                        debug!(file_name = %filename, "Ignoring non-matching file");
                    }
                }
            }
        }

        valid_files.sort_by(|a, b| {
            let a_timestamp = a.chars().rev().skip(5).take(19).collect::<String>();
            let b_timestamp = b.chars().rev().skip(5).take(19).collect::<String>();
            a_timestamp.cmp(&b_timestamp)
        });

        info!(
            session_id = %self.session_id,
            file_count = valid_files.len(),
            "Found valid task files"
        );

        Ok(valid_files)
    }
}
