#!/usr/bin/env python3
"""asmith - A Matrix To-Do List Bot.

This script implements a Matrix bot that helps manage to-do lists in Matrix channels.
"""

import argparse
import json
import logging
import os
import sys
import uuid
import asyncio
from datetime import datetime, timezone
from pathlib import Path
from typing import Dict, List, Optional, Tuple, Union
import re

from nio import AsyncClient, MatrixRoom, RoomMessageText, LoginError, SyncError, exceptions

# Application information from environment variables
APP_NAME = os.getenv("ASMITH_APP_NAME", "asmith")
APP_VERSION = os.getenv("ASMITH_APP_VERSION", "dev")

# Configure logging
logging.basicConfig(
    level=logging.DEBUG,
    format="%(asctime)s - %(name)s - %(levelname)s - %(message)s",
    handlers=[logging.StreamHandler()],
)
logger = logging.getLogger("asmith")


# Constants
class TaskEvent:
    """Constants for task events."""

    CREATED = "task_created"
    STATUS_UPDATED = "task_status_updated"
    LOG_ADDED = "task_log_added"
    TITLE_EDITED = "task_title_edited"


# Exceptions
class AsmithError(Exception):
    """Base exception for asmith errors."""

    pass


# Utility Functions
async def send_matrix_message(client: AsyncClient, room_id: str, message: str, html_message: Optional[str] = None) -> None:
    """Send a standardized Matrix message."""
    content = {
        "msgtype": "m.text",
        "body": message,
    }
    if html_message:
        content["format"] = "org.matrix.custom.html"
        content["formatted_body"] = html_message
    await client.room_send(
        room_id=room_id,
        message_type="m.room.message",
        content=content,
    )


class Task:
    """Represents a task in a to-do list."""

    def __init__(
        self,
        sender: str,
        id: int,
        title: str,
        status: str,
        logs: Optional[List[str]] = None,
        creator: Optional[str] = None,
    ) -> None:
        self.id: int = id
        self.title: str = title
        self.status: str = status
        self.logs: List[str] = logs or []
        self.internal_logs: List[Tuple[str, str, str]] = []  # (timestamp, user, log)
        self.creator: str = creator or sender
        self.add_internal_log(sender, TaskEvent.CREATED)

    def add_internal_log(
        self, sender: str, log: str, extra_info: str = ""
    ) -> None:
        timestamp = datetime.now().strftime("%Y-%m-%d %H:%M:%S")
        user = sender
        action = log if not extra_info else f"{log}: {extra_info}"
        self.internal_logs.append((timestamp, user, action))

    def add_log(self, sender: str, log: str) -> None:
        self.logs.append(log)
        self.add_internal_log(
            sender, TaskEvent.LOG_ADDED, f"'{log[:30]}{'...' if len(log) > 30 else ''}'"
        )

    def set_status(self, sender: str, status: str) -> None:
        old_status = self.status
        self.status = status
        self.add_internal_log(
            sender, TaskEvent.STATUS_UPDATED, f"from '{old_status}' to '{status}'"
        )

    def set_title(self, sender: str, title: str) -> None:
        old_title = self.title
        self.title = title
        self.add_internal_log(
            sender,
            TaskEvent.TITLE_EDITED,
            f"from '{old_title[:30]}{'...' if len(old_title) > 30 else ''}' to '{title[:30]}{'...' if len(title) > 30 else ''}'",
        )

    def show_details(self) -> str:
        details = [f"**[{self.status}] {self.title}**"]
        details.append(f"Created by: {self.creator}")

        # Add task logs if any
        if self.logs:
            details.append("\n**Logs:**")
            for i, log in enumerate(self.logs, 1):
                details.append(f"{i}. {log}")

        # Add history from internal logs
        if self.internal_logs:
            details.append("\n**History:**")
            for timestamp, user, action in self.internal_logs:
                # Extract the basic action type
                action_type = action.split(":", 1)[0] if ":" in action else action
                action_details = (
                    action.split(":", 1)[1].strip() if ":" in action else ""
                )

                # Convert action code to readable text
                readable_action = action_type
                if action_type == TaskEvent.CREATED:
                    readable_action = "Created task"
                elif action_type == TaskEvent.STATUS_UPDATED:
                    readable_action = f"Updated status {action_details}"
                elif action_type == TaskEvent.LOG_ADDED:
                    readable_action = f"Added log {action_details}"
                elif action_type == TaskEvent.TITLE_EDITED:
                    readable_action = f"Edited title {action_details}"

                details.append(f"• {timestamp} - {user}: {readable_action}")

        return "\n".join(details)

    def __str__(self) -> str:
        return f"**[{self.status}] {self.title}**"


class StorageManager:
    """Manages task persistence."""

    def __init__(self, data_dir: Union[str, Path], session_id: str) -> None:
        self.data_dir = Path(data_dir)
        self.session_id = session_id
        self.todo_lists: Dict[str, List[Task]] = {}  # room_id -> [Task, Task, ...]
        # Regex to validate save file names: APP_NAME_SESSIONID_YYYY-MM-DD_HH-MM-SS.json
        self.filename_pattern = re.compile(
            rf"^{re.escape(APP_NAME)}_.+_[0-9]{{4}}-[0-9]{{2}}-[0-9]{{2}}_[0-9]{{2}}-[0-9]{{2}}-[0-9]{{2}}Z\.json$"
        )

        if not self.data_dir.exists():
            self.data_dir.mkdir(parents=True)
            logger.info(f"Created data directory: {self.data_dir}")

    async def save(self) -> str:
        current_time = datetime.now(timezone.utc)
        filename = f"{APP_NAME}_{self.session_id}_{current_time.strftime('%Y-%m-%d_%H-%M-%SZ')}.json"
        filepath = self.data_dir / filename

        with open(filepath, "w") as f:
            json.dump(self.todo_lists, f, default=lambda o: o.__dict__, indent=2)

        return filename

    async def load(self, filename: str) -> bool:
        # Validate filename format
        if not self.filename_pattern.match(filename):
            logger.error(
                f"Attempted to load file with invalid format: {filename}. "
                f"Expected format: {APP_NAME}_<session_id>_<YYYY-MM-DD_HH-MM-SS>Z.json"
            )
            return False

        try:
            filepath = self.data_dir / filename
            with open(filepath, "r") as f:
                data = json.load(f)

            reconstructed_todo_lists: Dict[str, List[Task]] = {}

            for room_id, tasks in data.items():
                reconstructed_todo_lists[room_id] = []

                for task_data in tasks:
                    task = Task(
                        task_data.get("creator", "Unknown"), # Use creator as sender for reconstruction
                        task_data["id"],
                        task_data["title"],
                        task_data["status"],
                        task_data.get("logs", []),
                        task_data.get("creator", "Unknown"),
                    )

                    if "internal_logs" in task_data:
                        task.internal_logs = task_data["internal_logs"]

                    reconstructed_todo_lists[room_id].append(task)

            self.todo_lists = reconstructed_todo_lists
            return True

        except Exception as e:
            logger.error(f"Error loading todo lists: {e}")
            return False

    def list_saved_files(self) -> List[str]:
        valid_files = []
        for f in os.listdir(self.data_dir):
            if self.filename_pattern.match(f):
                valid_files.append(f)

        # Sort files based on the timestamp in the filename (YYYY-MM-DD_HH-MM-SS)
        # which is the 19 characters before ".json"
        valid_files.sort(key=lambda x: x[-24:-5])
        return valid_files


class TodoList:
    """Task management commands."""

    def __init__(self, client: AsyncClient, storage: StorageManager) -> None:
        self.client = client
        self.storage = storage

    async def add_task(self, room_id: str, sender: str, task_title: str) -> None:
        if room_id not in self.storage.todo_lists:
            self.storage.todo_lists[room_id] = []

        task_id = len(self.storage.todo_lists[room_id])
        new_task = Task(sender, task_id, task_title, "pending", [])
        self.storage.todo_lists[room_id].append(new_task)

        message = f"✅ Task Added: **{new_task.title}**"
        html_message = f"✅ Task Added: <b>{new_task.title}</b>"
        await send_matrix_message(self.client, room_id, message, html_message)
        await self.storage.save()

    async def list_tasks(self, room_id: str) -> None:
        tasks = self.storage.todo_lists.get(room_id, [])

        if not tasks:
            message = "ℹ️ Info: There are no tasks in this room's to-do list."
            await send_matrix_message(self.client, room_id, message)
            return

        response = ""
        for idx, task in enumerate(tasks, start=1):
            response += f"{idx}. {task}\n"

        message = f"📋 Room To-Do List:\n{response}"
        html_message = f"📋 Room To-Do List:<br>{response.replace('\n', '<br>')}"
        await send_matrix_message(self.client, room_id, message, html_message)

    async def done_task(self, room_id: str, sender: str, task_number: int) -> None:
        tasks = self.storage.todo_lists.get(room_id, [])

        if not tasks:
            message = "ℹ️ Info: There are no tasks in this room's to-do list."
            await send_matrix_message(self.client, room_id, message)
            return

        if 0 < task_number <= len(tasks):
            removed = tasks.pop(task_number - 1)
            removed.set_status(sender, "done")

            message = f"✔️ Task Marked as Done: **{removed}**"
            html_message = f"✔️ Task Marked as Done: <b>{removed}</b>"
            await send_matrix_message(self.client, room_id, message, html_message)
            await self.storage.save()
        else:
            message = f"❌ Error: Invalid task number: {task_number}. Use `!list` to see valid numbers."
            await send_matrix_message(self.client, room_id, message)

    async def close_task(self, room_id: str, sender: str, task_number: int) -> None:
        tasks = self.storage.todo_lists.get(room_id, [])

        if not tasks:
            message = "ℹ️ Info: There are no tasks in this room's to-do list."
            await send_matrix_message(self.client, room_id, message)
            return

        if 0 < task_number <= len(tasks):
            removed = tasks.pop(task_number - 1)
            removed.set_status(sender, "closed")

            message = f"✖️ Task Closed: **{removed}**"
            html_message = f"✖️ Task Closed: <b>{removed}</b>"
            await send_matrix_message(self.client, room_id, message, html_message)
            await self.storage.save()
        else:
            message = f"❌ Error: Invalid task number: {task_number}. Use `!list` to see valid numbers."
            await send_matrix_message(self.client, room_id, message)

    async def log_task(self, room_id: str, sender: str, task_number: int, log: str) -> None:
        tasks = self.storage.todo_lists.get(room_id, [])

        if not tasks:
            message = "ℹ️ Info: There are no tasks in this room's to-do list."
            await send_matrix_message(self.client, room_id, message)
            return

        if 0 < task_number <= len(tasks):
            task = tasks[task_number - 1]
            task.add_log(sender, log)

            message = f"📝 Log Added to Task #{task_number}:\nLog: '{log}'\n\nCurrent Task Details:\n{task.show_details()}"
            html_message = f"📝 Log Added to Task #{task_number}:<br>Log: '{log}'<br><br><b>Current Task Details:</b><br>{task.show_details().replace('\n', '<br>')}"
            await send_matrix_message(self.client, room_id, message, html_message)
            await self.storage.save()
        else:
            message = f"❌ Error: Invalid task number: {task_number}. Use `!list` to see valid numbers."
            await send_matrix_message(self.client, room_id, message)

    async def details_task(self, room_id: str, task_number: int) -> None:
        tasks = self.storage.todo_lists.get(room_id, [])

        if not tasks:
            message = "ℹ️ Info: There are no tasks in this room's to-do list."
            await send_matrix_message(self.client, room_id, message)
            return

        if 0 < task_number <= len(tasks):
            task = tasks[task_number - 1]
            details = task.show_details()

            message = f"🔍 Task #{task_number} Details:\n{details}"
            html_message = f"🔍 Task #{task_number} Details:<br>{details.replace('\n', '<br>')}"
            await send_matrix_message(self.client, room_id, message, html_message)
        else:
            message = f"❌ Error: Invalid task number: {task_number}. Use `!list` to see valid numbers."
            await send_matrix_message(self.client, room_id, message)

    async def edit_task(self, room_id: str, sender: str, task_number: int, new_title: str) -> None:
        tasks = self.storage.todo_lists.get(room_id, [])

        if not tasks:
            message = "ℹ️ Info: There are no tasks in this room's to-do list."
            await send_matrix_message(self.client, room_id, message)
            return

        if 0 < task_number <= len(tasks):
            task = tasks[task_number - 1]
            old_title = task.title
            task.set_title(sender, new_title)

            message = f"✏️ Task Edited: Task #{task_number} title changed:\nFrom: {old_title}\nTo: {new_title}"
            html_message = f"✏️ Task Edited: Task #{task_number} title changed:<br><b>From:</b> {old_title}<br><b>To:</b> {new_title}"
            await send_matrix_message(self.client, room_id, message, html_message)
            await self.storage.save()
        else:
            message = f"❌ Error: Invalid task number: {task_number}. Use `!list` to see valid numbers."
            await send_matrix_message(self.client, room_id, message)


class BotManagement:
    """Administrative Bot commands."""

    def __init__(self, client: AsyncClient, storage: StorageManager) -> None:
        self.client = client
        self.storage = storage

    async def clear_tasks(self, room_id: str) -> None:
        if room_id in self.storage.todo_lists and self.storage.todo_lists[room_id]:
            self.storage.todo_lists[room_id] = []
            message = "🗑️ List Cleared: The room's to-do list has been cleared."
            await send_matrix_message(self.client, room_id, message)
            await self.storage.save()
        else:
            message = "ℹ️ Info: There are no tasks in this room's to-do list to clear."
            await send_matrix_message(self.client, room_id, message)

    async def save_command(self, room_id: str) -> None:
        try:
            filename = await self.storage.save()
            message = f"💾 Lists Saved: The to-do lists have been saved to `{filename}`."
            html_message = f"💾 Lists Saved: The to-do lists have been saved to <code>{filename}</code>."
            await send_matrix_message(self.client, room_id, message, html_message)
        except Exception as e:
            logger.error(f"Error during save command: {e}", exc_info=True)
            message = f"❌ Error Saving: An error occurred while saving the lists: {e}"
            await send_matrix_message(self.client, room_id, message)

    async def load_command(self, room_id: str, filename: str) -> None:
        # Basic validation to prevent path traversal
        if ".." in filename or "/" in filename:
            message = "❌ Invalid Filename: Invalid characters detected in filename."
            await send_matrix_message(self.client, room_id, message)
            return

        # Validate filename format using the regex from StorageManager
        if not self.storage.filename_pattern.match(filename):
            message = (
                f"❌ Invalid Filename Format: Filename '{filename}' does not match the expected format: "
                f"`{APP_NAME}_<session_id>_<YYYY-MM-DD_HH-MM-SS>Z.json`"
            )
            html_message = (
                f"❌ Invalid Filename Format: Filename '<code>{filename}</code>' does not match the expected format: "
                f"<code>{APP_NAME}_<session_id>_<YYYY-MM-DD_HH-MM-SS>Z.json</code>"
            )
            await send_matrix_message(self.client, room_id, message, html_message)
            return

        success = await self.storage.load(filename)
        if success:
            message = f"📂 Lists Loaded: Successfully loaded to-do lists from `{filename}`."
            html_message = f"📂 Lists Loaded: Successfully loaded to-do lists from <code>{filename}</code>."
            await send_matrix_message(self.client, room_id, message, html_message)
        else:
            message = f"❌ Error Loading: Failed to load lists from `{filename}`. Check the filename and ensure it's a valid save file."
            html_message = f"❌ Error Loading: Failed to load lists from <code>{filename}</code>. Check the filename and ensure it's a valid save file."
            await send_matrix_message(self.client, room_id, message, html_message)

    async def loadlast_command(self, room_id: str) -> None:
        files = self.storage.list_saved_files()

        if not files:
            message = "ℹ️ No Files Found: No saved to-do list files found."
            await send_matrix_message(self.client, room_id, message)
            return

        # Files are already sorted by creation time, so the last one is the most recent
        most_recent_file = files[-1]

        success = await self.storage.load(most_recent_file)
        if success:
            message = f"📂 Last List Loaded: Successfully loaded the most recent lists from `{most_recent_file}`."
            html_message = f"📂 Last List Loaded: Successfully loaded the most recent lists from <code>{most_recent_file}</code>."
            await send_matrix_message(self.client, room_id, message, html_message)
        else:
            message = f"❌ Error Loading: Failed to load the most recent lists from `{most_recent_file}`. The file might be corrupted."
            html_message = f"❌ Error Loading: Failed to load the most recent lists from <code>{most_recent_file}</code>. The file might be corrupted."
            await send_matrix_message(self.client, room_id, message, html_message)

    async def list_files_command(self, room_id: str) -> None:
        try:
            files = self.storage.list_saved_files()
            if files:
                files_list = "\n".join([f"{i + 1}. `{f}`" for i, f in enumerate(files)])
                html_files_list = "<br>".join([f"{i + 1}. <code>{f}</code>" for i, f in enumerate(files)])
                message = f"📄 Available Save Files:\n{files_list}"
                html_message = f"📄 Available Save Files:<br>{html_files_list}"
                await send_matrix_message(self.client, room_id, message, html_message)
            else:
                message = "ℹ️ No Files Found: No saved to-do list files found."
                await send_matrix_message(self.client, room_id, message)
        except Exception as e:
            logger.error(f"Error listing files: {e}", exc_info=True)
            message = f"❌ Error Listing Files: An error occurred while listing saved files: {e}"
            await send_matrix_message(self.client, room_id, message)


class ConnectionMonitor:
    """Monitors connection health and failures."""

    def __init__(self, max_retries: int = 3) -> None:
        self.max_retries = max_retries
        self.consecutive_failures = 0
        self.total_failures = 0
        self.failure_types = {}
        self.last_failure_time = None
        self.first_failure_time = None

    def connection_successful(self) -> None:
        if self.consecutive_failures > 0:
            logger.info(
                f"Connection restored after {self.consecutive_failures} consecutive failures"
            )
        self.consecutive_failures = 0

    def connection_failed(self, error_type: str) -> bool:
        now = datetime.now()
        if self.consecutive_failures == 0:
            self.first_failure_time = now

        self.consecutive_failures += 1
        self.total_failures += 1
        self.last_failure_time = now

        # Track types of failures
        if error_type in self.failure_types:
            self.failure_types[error_type] += 1
        else:
            self.failure_types[error_type] = 1

        # Log detailed failure info
        elapsed = None
        if self.first_failure_time:
            elapsed_seconds = (now - self.first_failure_time).total_seconds()
            elapsed = f"{elapsed_seconds:.1f} seconds"

        logger.warning(
            f"Connection failure #{self.consecutive_failures}: {error_type}. "
            f"Total failures: {self.total_failures}"
            + (f" in {elapsed}" if elapsed else "")
        )

        # Critical errors that should cause immediate exit
        critical_errors = [
            "LoginError",
            "SyncError",
            "LocalProtocolError", # nio.exceptions.LocalProtocolError
            "OlmUnpickleError", # nio.exceptions.OlmUnpickleError
            "EncryptionError", # nio.exceptions.EncryptionError
        ]
        if error_type in critical_errors and self.consecutive_failures >= 2:
            logger.critical(
                f"Critical connection error: {error_type}. Exiting immediately."
            )
            return True

        # Check if max retries reached
        if self.consecutive_failures >= self.max_retries:
            logger.critical(
                f"Maximum connection retries ({self.max_retries}) reached. "
                f"Failure types: {self.failure_types}"
            )
            return True

        return False

    def get_status_report(self) -> str:
        if self.total_failures == 0:
            return "No connection failures detected"

        status = [
            f"Connection Status Report:",
            f"- Total failures: {self.total_failures}",
            f"- Consecutive failures: {self.consecutive_failures}",
        ]

        if self.first_failure_time:
            status.append(
                f"- First failure: {self.first_failure_time.strftime('%Y-%m-%d %H:%M:%S')}"
            )

        if self.last_failure_time:
            status.append(
                f"- Latest failure: {self.last_failure_time.strftime('%Y-%m-%d %H:%M:%S')}"
            )

        if self.first_failure_time and self.last_failure_time:
            elapsed_seconds = (
                self.last_failure_time - self.first_failure_time
            ).total_seconds()
            status.append(f"- Problem duration: {elapsed_seconds:.1f} seconds")

        status.append("- Failure types:")

        if not self.failure_types:
            status.append("  - None recorded")
        else:
            for error_type, count in sorted(
                self.failure_types.items(), key=lambda x: x[1], reverse=True
            ):
                percentage = (count / self.total_failures) * 100
                status.append(f"  - {error_type}: {count} ({percentage:.1f}%)")

        return "\n".join(status)


# Command line argument parsing
def parse_args() -> argparse.Namespace:
    """Parse command line arguments."""
    parser = argparse.ArgumentParser(
        description=f"{APP_NAME} - A Matrix To-Do List Bot"
    )
    parser.add_argument(
        "--data_dir",
        default="./data",
        help="Directory to store data files (default: ./data)",
    )
    parser.add_argument(
        "--homeserver",
        help="Matrix homeserver URL (e.g., https://matrix.org)",
    )
    parser.add_argument(
        "--user_id",
        help="Matrix user ID (e.g., @username:matrix.org)",
    )
    parser.add_argument(
        "--password",
        help="Matrix user password (can also be set via MATRIX_PASSWORD env variable)",
    )
    parser.add_argument(
        "--access_token",
        help="Matrix access token (can also be set via MATRIX_ACCESS_TOKEN env variable). Overrides password.",
    )
    parser.add_argument(
        "--debug", action="store_true", help="Enable debug mode with verbose logging"
    )
    parser.add_argument(
        "--max_retries",
        type=int,
        default=3,
        help="Maximum number of consecutive connection failures before exiting (default: 3)",
    )
    parser.add_argument(
        "--version",
        action="store_true",
        help=f"Show {APP_NAME} version information and exit",
    )

    return parser.parse_args()


def get_matrix_credentials(args: argparse.Namespace) -> Tuple[Optional[str], Optional[str], Optional[str], Optional[str]]:
    """Get Matrix credentials from args or environment."""
    homeserver = args.homeserver
    user_id = args.user_id
    password = args.password or os.getenv("MATRIX_PASSWORD")
    access_token = args.access_token or os.getenv("MATRIX_ACCESS_TOKEN")

    return homeserver, user_id, password, access_token


# Setup and start the bot
async def setup_bot(args, connection_monitor):
    """Set up the Matrix bot with all event handlers and cogs."""
    homeserver, user_id, password, access_token = get_matrix_credentials(args)

    if not homeserver or not user_id:
        logger.error("Homeserver URL and User ID are required.")
        sys.exit(1)

    client = AsyncClient(homeserver, user_id)

    if access_token:
        client.access_token = access_token
        logger.info("Using access token for authentication.")
    elif password:
        try:
            logger.info("Attempting to login with password...")
            response = await client.login(password)
            logger.info(f"Login successful: {response.device_id}")
        except LoginError as e:
            logger.critical(f"Failed to login to Matrix: {e}")
            sys.exit(1)
    else:
        logger.error("No password or access token provided for Matrix login.")
        sys.exit(1)

    # Initialize storage
    data_dir = Path(args.data_dir)
    session_id = str(uuid.uuid4()) # Generate session ID here
    storage = StorageManager(data_dir, session_id)

    # Initialize command handlers
    todo_list_commands = TodoList(client, storage)
    bot_management_commands = BotManagement(client, storage)

    # Define event handlers
    async def on_ready_matrix() -> None:
        connection_monitor.connection_successful()
        logger.info(f"Logged in as {client.user_id}")
        logger.info(f"Device ID: {client.device_id}")
        logger.info("Session ID: " + session_id)

        logger.info(f"Starting {APP_NAME} v{APP_VERSION}")

        # Auto-execute loadlast command if there are saved files
        files = storage.list_saved_files()
        if files:
            most_recent_file = files[-1]
            logger.info(f"Auto-loading last saved state from {most_recent_file}...")
            success = await storage.load(most_recent_file)
            if success:
                logger.info(f"Successfully auto-loaded state from {most_recent_file}")
            else:
                logger.error(f"Failed to auto-load state from {most_recent_file}")
        else:
            logger.info("No saved files found for auto-loading.")

    async def on_message_matrix(room: MatrixRoom, event: RoomMessageText) -> None:
        if event.sender == client.user_id:
            return # Don't react to our own messages

        logger.debug(f"Message from {event.sender} in {room.display_name} ({room.room_id}): {event.body}")

        # Simple command parsing
        if event.body.startswith("!"):
            parts = event.body.split(maxsplit=1)
            command = parts[0][1:].lower()
            args_str = parts[1] if len(parts) > 1 else ""

            try:
                if command == "add" or command == "a":
                    await todo_list_commands.add_task(room.room_id, event.sender, args_str)
                elif command == "list" or command == "ls" or command == "l":
                    await todo_list_commands.list_tasks(room.room_id)
                elif command == "done" or command == "d":
                    task_number = int(args_str.strip())
                    await todo_list_commands.done_task(room.room_id, event.sender, task_number)
                elif command == "close" or command == "c":
                    task_number = int(args_str.strip())
                    await todo_list_commands.close_task(room.room_id, event.sender, task_number)
                elif command == "log" or command == "lg":
                    task_number_str, log_content = args_str.split(maxsplit=1)
                    task_number = int(task_number_str.strip())
                    await todo_list_commands.log_task(room.room_id, event.sender, task_number, log_content)
                elif command == "details" or command == "det":
                    task_number = int(args_str.strip())
                    await todo_list_commands.details_task(room.room_id, task_number)
                elif command == "edit" or command == "e":
                    task_number_str, new_title_content = args_str.split(maxsplit=1)
                    task_number = int(task_number_str.strip())
                    await todo_list_commands.edit_task(room.room_id, event.sender, task_number, new_title_content)
                elif command == "clear" or command == "clr":
                    await bot_management_commands.clear_tasks(room.room_id)
                elif command == "save" or command == "s":
                    await bot_management_commands.save_command(room.room_id)
                elif command == "load" or command == "ld":
                    await bot_management_commands.load_command(room.room_id, args_str.strip())
                elif command == "loadlast" or command == "ll":
                    await bot_management_commands.loadlast_command(room.room_id)
                elif command == "list_files" or command == "lf":
                    await bot_management_commands.list_files_command(room.room_id)
                elif command == "help" or command == "h":
                    help_message = (
                        f"**{APP_NAME} Bot Commands:**\n"
                        f"`!add <task>`: Add a new task.\n"
                        f"`!list`: List all tasks.\n"
                        f"`!done <task_number>`: Mark a task as done and remove it.\n"
                        f"`!close <task_number>`: Close a task without completing and remove it.\n"
                        f"`!log <task_number> <note>`: Add a note to a task.\n"
                        f"`!details <task_number>`: Show details of a task.\n"
                        f"`!edit <task_number> <new_title>`: Edit a task's title.\n"
                        f"`!clear`: Clear all tasks in the current room.\n"
                        f"`!save`: Manually save the current state.\n"
                        f"`!load <filename>`: Load state from a file.\n"
                        f"`!loadlast`: Load the most recently saved state.\n"
                        f"`!list_files`: List all saved files.\n"
                        f"`!help`: Show this help message."
                    )
                    html_help_message = (
                        f"<b>{APP_NAME} Bot Commands:</b><br>"
                        f"<code>!add <task></code>: Add a new task.<br>"
                        f"<code>!list</code>: List all tasks.<br>"
                        f"<code>!done <task_number></code>: Mark a task as done and remove it.<br>"
                        f"<code>!close <task_number></code>: Close a task without completing and remove it.<br>"
                        f"<code>!log <task_number> <note></code>: Add a note to a task.<br>"
                        f"<code>!details <task_number></code>: Show details of a task.<br>"
                        f"<code>!edit <task_number> <new_title></code>: Edit a task's title.<br>"
                        f"<code>!clear</code>: Clear all tasks in the current room.<br>"
                        f"<code>!save</code>: Manually save the current state.<br>"
                        f"<code>!load <filename></code>: Load state from a file.<br>"
                        f"<code>!loadlast</code>: Load the most recently saved state.<br>"
                        f"<code>!list_files</code>: List all saved files.<br>"
                        f"<code>!help</code>: Show this help message."
                    )
                    await send_matrix_message(client, room.room_id, help_message, html_help_message)
                else:
                    await send_matrix_message(client, room.room_id, f"Unknown command: `{command}`. Type `!help` for a list of commands.")
            except ValueError:
                await send_matrix_message(client, room.room_id, f"Invalid argument for command `!{command}`. Please check the usage with `!help {command}`.")
            except Exception as e:
                logger.error(f"Error processing command '{command}': {e}", exc_info=True)
                await send_matrix_message(client, room.room_id, f"An error occurred while processing your command: {e}")

    client.add_event_callback(on_message_matrix, RoomMessageText)
    client.add_event_callback(lambda _: on_ready_matrix(), SyncError) # This is a placeholder for on_ready equivalent

    return client, storage


async def main() -> None:
    """Main entry point for the application."""
    # Parse arguments
    args = parse_args()

    # Check if version flag was set
    if args.version:
        print(f"{APP_NAME} v{APP_VERSION}")
        sys.exit(0)

    # Configure logging level
    if args.debug:
        logging.getLogger().setLevel(logging.DEBUG)
        logger.debug("Debug mode enabled")

    # Get Matrix credentials
    homeserver, user_id, password, access_token = get_matrix_credentials(args)
    if not homeserver or not user_id or (not password and not access_token):
        logger.error(
            "Missing Matrix credentials. Please provide --homeserver, --user_id, and either --password or --access_token, or set MATRIX_PASSWORD/MATRIX_ACCESS_TOKEN environment variables."
        )
        sys.exit(1)

    # Initialize connection monitor
    connection_monitor = ConnectionMonitor(max_retries=args.max_retries)
    logger.info(
        "Connection monitor initialized with max_retries=" + str(args.max_retries)
    )

    # Set up the bot
    client, _storage = await setup_bot(args, connection_monitor)

    # Run the bot
    try:
        logger.info("Starting bot sync...")
        # The `sync_forever` method will block until the client is stopped
        await client.sync_forever(timeout=30000, callback=connection_monitor.connection_successful)
    except (LoginError, SyncError, exceptions.LocalProtocolError, exceptions.OlmUnpickleError, exceptions.EncryptionError) as e:
        error_type = type(e).__name__
        logger.exception(f"Matrix client error: {error_type}: {e}")
        if connection_monitor.connection_failed(error_type):
            logger.critical("Connection failure threshold reached. Exiting...")
            logger.critical(connection_monitor.get_status_report())
        sys.exit(1)
    except Exception as e:
        logger.exception(f"An unexpected error occurred: {e}")
        sys.exit(1)
    finally:
        await client.close()
        logger.info("Matrix client closed.")


if __name__ == "__main__":
    asyncio.run(main())
