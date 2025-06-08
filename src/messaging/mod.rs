use anyhow::Result;
use async_trait::async_trait;
use matrix_sdk::ruma::OwnedRoomId;

/// MessageSender trait provides an abstraction for sending messages to rooms
/// This decouples the task management logic from matrix-specific implementation details
#[async_trait]
pub trait MessageSender: Send + Sync {
    /// Send a plain text message to a room
    async fn send_text_message(&self, room_id: &OwnedRoomId, message: &str) -> Result<()>;

    /// Send a formatted HTML message to a room
    async fn send_formatted_message(
        &self,
        room_id: &OwnedRoomId,
        text: &str,
        html: &str,
    ) -> Result<()>;

    /// Send a response message that can be either plain text or HTML
    async fn send_response(
        &self,
        room_id: &OwnedRoomId,
        message: &str,
        html_message: Option<String>,
    ) -> Result<()>;
}

/// Implements the MessageSender trait for Matrix client
pub struct MatrixMessageSender {
    client: matrix_sdk::Client,
}

impl MatrixMessageSender {
    pub fn new(client: matrix_sdk::Client) -> Self {
        Self { client }
    }
}

#[async_trait]
impl MessageSender for MatrixMessageSender {
    async fn send_text_message(&self, room_id: &OwnedRoomId, message: &str) -> Result<()> {
        let room = self
            .client
            .get_room(room_id)
            .ok_or_else(|| anyhow::anyhow!("Room not found"))?;

        // Create a plain text message type
        let content =
            matrix_sdk::ruma::events::room::message::RoomMessageEventContent::notice_plain(message);
        room.send(content)
            .await
            .map_err(|e| anyhow::anyhow!("{:?}", e))?;

        Ok(())
    }

    async fn send_formatted_message(
        &self,
        room_id: &OwnedRoomId,
        text: &str,
        html: &str,
    ) -> Result<()> {
        let room = self
            .client
            .get_room(room_id)
            .ok_or_else(|| anyhow::anyhow!("Room not found"))?;

        // Create HTML formatted message content
        let content_type = matrix_sdk::ruma::events::room::message::MessageType::notice_html(
            text.to_string(),
            html.to_string(),
        );
        let content =
            matrix_sdk::ruma::events::room::message::RoomMessageEventContent::new(content_type);

        room.send(content)
            .await
            .map_err(|e| anyhow::anyhow!("{:?}", e))?;

        Ok(())
    }

    async fn send_response(
        &self,
        room_id: &OwnedRoomId,
        message: &str,
        html_message: Option<String>,
    ) -> Result<()> {
        if let Some(html) = html_message {
            self.send_formatted_message(room_id, message, &html).await
        } else {
            self.send_text_message(room_id, message).await
        }
    }
}
