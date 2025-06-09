use anyhow::{Context, Result, anyhow, bail};
use futures_util::stream::StreamExt;
use matrix_sdk::encryption::verification::Verification;
use matrix_sdk::ruma::OwnedDeviceId;
use matrix_sdk::ruma::events::room::{
    member::StrippedRoomMemberEvent, message::OriginalSyncRoomMessageEvent,
};
use matrix_sdk::ruma::events::{
    ToDeviceEvent,
    key::verification::{
        cancel::ToDeviceKeyVerificationCancelEventContent,
        done::ToDeviceKeyVerificationDoneEventContent, key::ToDeviceKeyVerificationKeyEventContent,
        mac::ToDeviceKeyVerificationMacEventContent,
        request::ToDeviceKeyVerificationRequestEventContent,
        start::ToDeviceKeyVerificationStartEventContent,
    },
};
use matrix_sdk::{
    Client, Room, RoomState, SessionMeta, SessionTokens, authentication::matrix::MatrixSession,
    config::SyncSettings,
};
use ruma::DeviceId;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use std::path::{Path, PathBuf};
use tokio::time::Duration;
use tracing::{debug, error, info, warn};

use crate::config::APP_NAME;

use rand::{Rng, rngs::ThreadRng};
use rand_distr::Alphanumeric;
use tokio::fs as async_fs; // For async file operations

// Configuration for the SQLite store
#[derive(Debug, Serialize, Deserialize, Clone)] // Added Clone
pub struct ClientStoreConfig {
    store_path: PathBuf,      // Full path to the SQLite file's directory
    store_passphrase: String, // Passphrase for encrypting the store
}

// Holds all data needed to persist and restore a session fully
#[derive(Debug, Serialize, Deserialize)]
pub struct PersistedSession {
    client_store_config: ClientStoreConfig,
    matrix_session: MatrixSession, // The SDK's session object
    sync_token: Option<String>,
}

pub async fn restore_session(
    session_file_path: &PathBuf,
    config: &crate::config::BotConfig, // Renamed from _config, will be used
) -> Result<(Client, Option<String>, ClientStoreConfig)> {
    info!(
        "Attempting to restore session from: {}",
        session_file_path.display()
    );

    let session_json = async_fs::read_to_string(session_file_path)
        .await
        .context(format!(
            "Failed to read session file: {}",
            session_file_path.display()
        ))?;

    let persisted_session: PersistedSession =
        serde_json::from_str(&session_json).context("Failed to deserialize session data")?;

    let client_store_config = persisted_session.client_store_config.clone();
    let matrix_session = persisted_session.matrix_session;
    let sync_token = persisted_session.sync_token;

    let homeserver_url = config
        .homeserver
        .as_ref()
        .ok_or_else(|| anyhow!("Homeserver URL not found in config during session restore"))?;
    info!(
        "Restoring client with homeserver: {}",
        homeserver_url.as_str()
    );
    info!(
        "Using store path: {}",
        client_store_config.store_path.display()
    );

    let client = Client::builder()
        .homeserver_url(homeserver_url.as_str())
        .sqlite_store(
            &client_store_config.store_path,
            Some(&client_store_config.store_passphrase),
        )
        .build()
        .await
        .context("Failed to build client during session restore")?;

    client
        .restore_session(matrix_session.clone()) // Restore full session state
        .await
        .context("Failed to restore Matrix session")?;

    info!(
        "Successfully restored session for user: {}",
        matrix_session.meta.user_id
    );
    Ok((client, sync_token, client_store_config))
}

pub async fn login_and_save_session(
    session_file_path: &PathBuf,
    store_base_path: &Path, // Base directory for all session stores
    config: &crate::config::BotConfig,
) -> Result<(Client, Option<String>, ClientStoreConfig)> {
    info!("Performing new login and creating new session store.");

    let homeserver_url_str = config.get_homeserver()?;

    // Create a unique directory for this session's store
    let mut rng = ThreadRng::default();
    let store_subdir_name: String = std::iter::repeat_with(|| rng.sample(Alphanumeric))
        .map(char::from)
        .take(16) // Increased length for more uniqueness
        .collect();
    let store_path = store_base_path.join(store_subdir_name);
    async_fs::create_dir_all(&store_path)
        .await
        .context(format!(
            "Failed to create store directory at {}",
            store_path.display()
        ))?;

    let store_passphrase: String = std::iter::repeat_with(|| rng.sample(Alphanumeric))
        .map(char::from)
        .take(32)
        .collect();

    info!(
        "Building client for new login. Homeserver: {}",
        homeserver_url_str.as_str()
    );
    info!("New SQLite store will be at: {}", store_path.display());

    let client_builder = Client::builder()
        .homeserver_url(homeserver_url_str.as_str())
        .sqlite_store(&store_path, Some(&store_passphrase)); // Specify server versions

    let client = client_builder
        .build()
        .await
        .context("Failed to build client for new login")?;

    // Perform login
    if let Some(token) = &config.access_token {
        tracing::info!("Attempting to log in with access token.");
        let user_id = config.get_user_id().context("User ID not found in config, but access token is present. User ID is required for token login.")?;

        let device_id: OwnedDeviceId = DeviceId::new();
        tracing::info!(
            "Generated new device ID for token login: {}",
            device_id.as_str()
        );

        let session_struct = MatrixSession {
            meta: SessionMeta {
                user_id: user_id.to_owned(), // user_id is &UserId, convert to OwnedUserId
                device_id,
            },
            tokens: SessionTokens {
                access_token: token.clone(),
                refresh_token: None, // BotConfig doesn't currently provide a refresh_token
            },
        };

        client
            .restore_session(session_struct)
            .await
            .context("Failed to restore session with token")?;
        tracing::info!("Successfully logged in with access token and restored session.");
    } else if let (Ok(user_id), Some(password)) = (config.get_user_id(), &config.password) {
        client
            .matrix_auth()
            .login_username(user_id.as_str(), password.as_str())
            .initial_device_display_name(APP_NAME)
            .send()
            .await
            .context("Login with username and password failed")?;
    } else {
        bail!(
            "Login failed: Ensure homeserver, user ID, and either password or access token are correctly configured."
        );
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
        .ok_or_else(|| anyhow!("Failed to get MatrixSession after login"))?;

    let client_store_config = ClientStoreConfig {
        store_path,
        store_passphrase,
    };

    let persisted_session_data = PersistedSession {
        client_store_config: client_store_config.clone(),
        matrix_session,
        sync_token: None, // Sync token is obtained after the first sync
    };

    let session_json = serde_json::to_string_pretty(&persisted_session_data)
        .context("Failed to serialize session data for saving")?;
    async_fs::write(session_file_path, session_json)
        .await
        .context(format!(
            "Failed to write session file to {}",
            session_file_path.display()
        ))?;

    info!("Session saved to: {}", session_file_path.display());
    Ok((client, None, client_store_config))
}

// Renamed and refactored from save_updated_session_details
pub async fn save_current_session(
    client: &Client,
    session_file_path: &PathBuf,
    client_store_config: &ClientStoreConfig, // Pass the existing store config
    current_sync_token: Option<String>,
) -> Result<()> {
    info!(
        "Attempting to save current session to: {}",
        session_file_path.display()
    );

    let matrix_session = client
        .matrix_auth()
        .session()
        .ok_or_else(|| anyhow!("Failed to get MatrixSession from client for saving"))?;

    let persisted_session_data = PersistedSession {
        client_store_config: client_store_config.clone(),
        matrix_session,
        sync_token: current_sync_token,
    };

    let session_json = serde_json::to_string_pretty(&persisted_session_data)
        .context("Failed to serialize current session data for saving")?;
    async_fs::write(session_file_path, session_json)
        .await
        .context(format!(
            "Failed to write current session file to {}",
            session_file_path.display()
        ))?;

    info!(
        "Successfully saved current session to: {}",
        session_file_path.display()
    );
    Ok(())
}

pub struct ConnectionMonitor {
    pub max_retries: usize,
    pub consecutive_failures: usize,
    pub total_failures: usize, // This field was present and should remain
    pub failure_types: HashMap<String, usize>, // This field was present and should remain
                               // last_failure_time and first_failure_time were intentionally removed
}

impl ConnectionMonitor {
    pub fn new(max_retries: usize) -> Self {
        Self {
            max_retries,
            consecutive_failures: 0,
            total_failures: 0,
            failure_types: HashMap::new(),
        }
    }

    pub fn connection_successful(&mut self) {
        if self.consecutive_failures > 0 {
            info!(
                "Connection restored after {} consecutive failures. Total overall failures: {}",
                self.consecutive_failures, self.total_failures
            );
        }
        self.consecutive_failures = 0;
    }

    pub fn connection_failed(&mut self, error_type: String) -> bool {
        self.total_failures += 1;
        *self.failure_types.entry(error_type.clone()).or_insert(0) += 1;
        self.consecutive_failures += 1;

        if self.consecutive_failures >= self.max_retries {
            warn!(
                "Max retries ({}) reached for error type: {}. Total failures for this type: {}, Total overall failures: {}",
                self.max_retries,
                error_type,
                self.failure_types.get(&error_type).unwrap_or(&0),
                self.total_failures
            );
            true // Indicate that max retries have been reached
        } else {
            info!(
                "Connection failed ({} of {} retries for error type: {}). Total failures for this type: {}, Total overall failures: {}",
                self.consecutive_failures,
                self.max_retries,
                error_type,
                self.failure_types.get(&error_type).unwrap_or(&0),
                self.total_failures
            );
            false // Indicate that max retries have not been reached
        }
    }
}

pub async fn handle_verification_events(client: Client) {
    info!("Setting up verification event handlers...");

    // Handler for m.key.verification.request
    client.add_event_handler(
        |ev: ToDeviceEvent<ToDeviceKeyVerificationRequestEventContent>, c: Client| async move {
            let sender = ev.sender;
            let flow_id_str = ev.content.transaction_id.to_string(); // Keep original flow_id from event for consistency if needed
            info!(%sender, flow_id = %flow_id_str, "Received m.key.verification.request");

            let encryption_instance = c.encryption(); // Direct assignment, not Option handling
            if let Some(request) = encryption_instance
                .get_verification_request(&sender, &flow_id_str) // Use flow_id_str here
                .await
            {
                info!(%sender, flow_id = %request.flow_id(), "Got SdkVerificationRequest. Accepting with SASv1...");
                if let Err(e) = request.accept().await {
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
        |ev: ToDeviceEvent<ToDeviceKeyVerificationStartEventContent>, c: Client| async move {
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
        |ev: ToDeviceEvent<ToDeviceKeyVerificationKeyEventContent>, c: Client| async move {
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
        |ev: ToDeviceEvent<ToDeviceKeyVerificationMacEventContent>, _c: Client| async move {
            let sender = ev.sender;
            let flow_id = ev.content.transaction_id.to_string();
            info!(%sender, %flow_id, "Received m.key.verification.mac. Keys: {:?}, MAC: {:?}", ev.content.keys, ev.content.mac);
            // Typically, the SDK handles this internally. We're just logging.
        },
    );
    info!("Registered handler for m.key.verification.mac");

    // Handler for m.key.verification.cancel
    client.add_event_handler(
        |ev: ToDeviceEvent<ToDeviceKeyVerificationCancelEventContent>, _c: Client| async move {
            let sender = ev.sender;
            let flow_id = ev.content.transaction_id.to_string();
            info!(%sender, %flow_id, "Received m.key.verification.cancel. Code: {}, Reason: {}", ev.content.code, ev.content.reason);
        },
    );
    info!("Registered handler for m.key.verification.cancel");

    // Handler for m.key.verification.done
    client.add_event_handler(
        |ev: ToDeviceEvent<ToDeviceKeyVerificationDoneEventContent>, _c: Client| async move {
            let sender = ev.sender;
            let flow_id = ev.content.transaction_id.to_string();
            info!(%sender, %flow_id, "Received m.key.verification.done");
        },
    );
    info!("Registered handler for m.key.verification.done");

    info!("All verification event handlers registered.");
}

pub async fn on_stripped_state_member(
    room_member: StrippedRoomMemberEvent,
    client: Client,
    room: Room,
) {
    if room_member.state_key != client.user_id().unwrap() {
        return;
    }

    info!("Autojoining room {}", room.room_id());
    let room_id = room.room_id();
    if let Err(e) = room.join().await {
        error!("Failed to join room {}: {}", room_id, e);
    } else {
        info!("Successfully joined room {}", room_id);
    }
}

pub fn register_message_handler(client: &Client) {
    // Register handler for room messages to process bot commands
    client.add_event_handler(
        // Closure for room messages
        move |ev: OriginalSyncRoomMessageEvent, room: Room, _client_clone: Client| async move {
            if room.state() != RoomState::Joined {
                return;
            }

            let bot_core_ref = crate::BOT_CORE
                .get()
                .expect("BOT_CORE not initialized")
                .clone();
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
                            }
                        }
                    }
                }
            });
        },
    );
    info!("Room message handler registered for command processing");
}

pub async fn start_sync_loop(
    client: Client,
    initial_sync_settings: SyncSettings, // Renamed for clarity
    connection_monitor: &mut ConnectionMonitor,
    session_file_path: &PathBuf,             // Added
    client_store_config: &ClientStoreConfig, // Added
) -> Result<()> {
    info!("Starting Matrix sync loop...");
    let mut current_sync_settings = initial_sync_settings;

    loop {
        info!("Initiating a sync cycle...");
        match client.sync_once(current_sync_settings.clone()).await {
            Ok(sync_response) => {
                connection_monitor.connection_successful();
                let new_sync_token = sync_response.next_batch;
                info!("Sync successful. New sync token: {}", new_sync_token);

                if let Err(save_err) = save_current_session(
                    &client,
                    session_file_path,
                    client_store_config,
                    Some(new_sync_token.clone()),
                )
                .await
                {
                    error!("Failed to save current session after sync: {:?}", save_err);
                    // Decide if this is a critical error. For now, we'll log and continue.
                }

                current_sync_settings = SyncSettings::default().token(new_sync_token);
            }
            Err(e) => {
                error!("Sync loop exited with error: {}", e);
                let should_exit =
                    connection_monitor.connection_failed(format!("Sync loop error: {}", e));
                if should_exit {
                    return Err(anyhow!(
                        "Connection monitor recommended exit due to critical errors"
                    ));
                }
                // Original error handling for sync failure from client.sync() is adapted here
                error!("Sync cycle failed: {}", e);
                let error_details = format!("Sync cycle error: {}", e);
                if connection_monitor.connection_failed(error_details) {
                    return Err(anyhow!(
                        "Connection monitor recommended exit due to critical sync errors."
                    ));
                }
                // If not exiting, the loop will continue, implicitly retrying the sync on the next iteration.
                // A delay might be useful here depending on the nature of expected errors.
                tokio::time::sleep(tokio::time::Duration::from_secs(5)).await; // Brief pause before retrying
            }
        }
    }
}
