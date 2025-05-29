#!/usr/bin/env python3
"""
Run all tests for the asmith Matrix bot.
"""

import unittest
import sys
import asyncio
from unittest.mock import AsyncMock, patch
from pathlib import Path
import tempfile
import shutil

# Import necessary classes from asmith.py
from asmith import (
    AsyncClient, # This is actually from nio, but we need it for type hinting in mocks
    MatrixRoom,
    RoomMessageText,
    TodoList,
    BotManagement,
    StorageManager,
    Task,
    send_matrix_message,
    APP_NAME,
)

class TestAsmithMatrixBot(unittest.IsolatedAsyncioTestCase):
    """
    Tests for the asmith Matrix bot functionality.
    """

    async def asyncSetUp(self):
        self.mock_client = AsyncMock(spec=AsyncClient)
        self.mock_client.user_id = "@testuser:matrix.org" # Mock user_id for bot's own messages

        # Create a temporary directory for storage
        self.temp_data_dir = Path(tempfile.mkdtemp())
        self.storage = StorageManager(self.temp_data_dir, "test_session")
        
        self.todo_list_commands = TodoList(self.mock_client, self.storage)
        self.bot_management_commands = BotManagement(self.mock_client, self.storage)

        self.room_id = "!testroom:matrix.org"
        self.sender = "@alice:matrix.org"

    async def asyncTearDown(self):
        # Clean up the temporary directory
        if self.temp_data_dir.exists():
            shutil.rmtree(self.temp_data_dir)

    @patch("asmith.send_matrix_message")
    async def test_add_task(self, mock_send_matrix_message):
        task_title = "Buy groceries"
        await self.todo_list_commands.add_task(self.room_id, self.sender, task_title)

        # Verify message was sent
        mock_send_matrix_message.assert_called_once_with(
            self.mock_client,
            self.room_id,
            f"✅ Task Added: **{task_title}**",
            f"✅ Task Added: <b>{task_title}</b>"
        )

        # Verify task was added to storage
        self.assertIn(self.room_id, self.storage.todo_lists)
        self.assertEqual(len(self.storage.todo_lists[self.room_id]), 1)
        task = self.storage.todo_lists[self.room_id][0]
        self.assertEqual(task.title, task_title)
        self.assertEqual(task.status, "pending")
        self.assertEqual(task.creator, self.sender)
        self.assertEqual(task.id, 0) # First task added should have ID 0

    @patch("asmith.send_matrix_message")
    async def test_list_tasks_empty(self, mock_send_matrix_message):
        await self.todo_list_commands.list_tasks(self.room_id)

        mock_send_matrix_message.assert_called_once_with(
            self.mock_client,
            self.room_id,
            "ℹ️ Info: There are no tasks in this room's to-do list."
        )
        self.assertNotIn(self.room_id, self.storage.todo_lists)

    @patch("asmith.send_matrix_message")
    async def test_list_tasks_with_items(self, mock_send_matrix_message):
        await self.todo_list_commands.add_task(self.room_id, self.sender, "Task 1")
        await self.todo_list_commands.add_task(self.room_id, self.sender, "Task 2")
        mock_send_matrix_message.reset_mock() # Reset mock after add_task calls

        await self.todo_list_commands.list_tasks(self.room_id)

        expected_message = (
            "📋 Room To-Do List:\n"
            "1. **[pending] Task 1**\n"
            "2. **[pending] Task 2**\n"
        )
        expected_html_message = (
            "📋 Room To-Do List:<br>"
            "1. <b>[pending] Task 1</b><br>"
            "2. <b>[pending] Task 2</b><br>"
        )
        mock_send_matrix_message.assert_called_once_with(
            self.mock_client,
            self.room_id,
            expected_message,
            expected_html_message
        )

    @patch("asmith.send_matrix_message")
    async def test_done_task(self, mock_send_matrix_message):
        await self.todo_list_commands.add_task(self.room_id, self.sender, "Task to be done")
        mock_send_matrix_message.reset_mock()

        await self.todo_list_commands.done_task(self.room_id, self.sender, 1)

        mock_send_matrix_message.assert_called_once_with(
            self.mock_client,
            self.room_id,
            "✔️ Task Marked as Done: **[done] Task to be done**",
            "✔️ Task Marked as Done: <b>[done] Task to be done</b>"
        )
        self.assertNotIn(self.room_id, self.storage.todo_lists) # Done tasks are removed from the list

    @patch("asmith.send_matrix_message")
    async def test_done_task_invalid_number(self, mock_send_matrix_message):
        await self.todo_list_commands.add_task(self.room_id, self.sender, "Task 1")
        mock_send_matrix_message.reset_mock()

        await self.todo_list_commands.done_task(self.room_id, self.sender, 99)

        mock_send_matrix_message.assert_called_once_with(
            self.mock_client,
            self.room_id,
            "❌ Error: Invalid task number: 99. Use `!list` to see valid numbers."
        )
        self.assertEqual(len(self.storage.todo_lists[self.room_id]), 1) # Task should still be there

    @patch("asmith.send_matrix_message")
    async def test_close_task(self, mock_send_matrix_message):
        await self.todo_list_commands.add_task(self.room_id, self.sender, "Task to be closed")
        mock_send_matrix_message.reset_mock()

        await self.todo_list_commands.close_task(self.room_id, self.sender, 1)

        mock_send_matrix_message.assert_called_once_with(
            self.mock_client,
            self.room_id,
            "✖️ Task Closed: **[closed] Task to be closed**",
            "✖️ Task Closed: <b>[closed] Task to be closed</b>"
        )
        self.assertNotIn(self.room_id, self.storage.todo_lists) # Closed tasks are removed from the list

    @patch("asmith.send_matrix_message")
    async def test_log_task(self, mock_send_matrix_message):
        await self.todo_list_commands.add_task(self.room_id, self.sender, "Task with log")
        mock_send_matrix_message.reset_mock()

        log_content = "This is a test log entry."
        await self.todo_list_commands.log_task(self.room_id, self.sender, 1, log_content)

        # Verify log was added to the task
        task = self.storage.todo_lists[self.room_id][0]
        self.assertIn(log_content, task.logs)
        self.assertEqual(len(task.internal_logs), 2) # Created + Log Added

        # Verify message was sent (check for partial match due to dynamic details)
        args, kwargs = mock_send_matrix_message.call_args
        self.assertIn(f"📝 Log Added to Task #1:\nLog: '{log_content}'", args[1])
        self.assertIn("Current Task Details:", args[1])
        self.assertIn(f"📝 Log Added to Task #1:<br>Log: '{log_content}'", kwargs['html_message'])
        self.assertIn("<b>Current Task Details:</b>", kwargs['html_message'])

    @patch("asmith.send_matrix_message")
    async def test_details_task(self, mock_send_matrix_message):
        await self.todo_list_commands.add_task(self.room_id, self.sender, "Detailed Task")
        task = self.storage.todo_lists[self.room_id][0]
        task.add_log(self.sender, "First log")
        task.set_status(self.sender, "in progress")
        mock_send_matrix_message.reset_mock()

        await self.todo_list_commands.details_task(self.room_id, 1)

        # Verify message was sent (check for partial match due to dynamic details)
        args, kwargs = mock_send_matrix_message.call_args
        self.assertIn("🔍 Task #1 Details:", args[1])
        self.assertIn("**[in progress] Detailed Task**", args[1])
        self.assertIn("Created by:", args[1])
        self.assertIn("Logs:", args[1])
        self.assertIn("History:", args[1])

    @patch("asmith.send_matrix_message")
    async def test_edit_task(self, mock_send_matrix_message):
        await self.todo_list_commands.add_task(self.room_id, self.sender, "Old Title")
        mock_send_matrix_message.reset_mock()

        new_title = "New and Improved Title"
        await self.todo_list_commands.edit_task(self.room_id, self.sender, 1, new_title)

        # Verify title was updated
        task = self.storage.todo_lists[self.room_id][0]
        self.assertEqual(task.title, new_title)
        self.assertEqual(len(task.internal_logs), 2) # Created + Title Edited

        mock_send_matrix_message.assert_called_once_with(
            self.mock_client,
            self.room_id,
            f"✏️ Task Edited: Task #1 title changed:\nFrom: Old Title\nTo: {new_title}",
            f"✏️ Task Edited: Task #1 title changed:<br><b>From:</b> Old Title<br><b>To:</b> {new_title}"
        )

    @patch("asmith.send_matrix_message")
    async def test_clear_tasks(self, mock_send_matrix_message):
        await self.todo_list_commands.add_task(self.room_id, self.sender, "Task 1")
        await self.todo_list_commands.add_task(self.room_id, self.sender, "Task 2")
        mock_send_matrix_message.reset_mock()

        await self.bot_management_commands.clear_tasks(self.room_id)

        mock_send_matrix_message.assert_called_once_with(
            self.mock_client,
            self.room_id,
            "🗑️ List Cleared: The room's to-do list has been cleared."
        )
        self.assertIn(self.room_id, self.storage.todo_lists)
        self.assertEqual(len(self.storage.todo_lists[self.room_id]), 0)

    @patch("asmith.send_matrix_message")
    async def test_save_command(self, mock_send_matrix_message):
        await self.todo_list_commands.add_task(self.room_id, self.sender, "Task to save")
        mock_send_matrix_message.reset_mock()

        await self.bot_management_commands.save_command(self.room_id)

        # Verify message was sent (check for partial match due to dynamic filename)
        args, kwargs = mock_send_matrix_message.call_args
        self.assertIn("💾 Lists Saved: The to-do lists have been saved to `", args[1])
        self.assertIn("`.json`.", args[1])
        self.assertIn("💾 Lists Saved: The to-do lists have been saved to <code>", kwargs['html_message'])
        self.assertIn("</code>.", kwargs['html_message'])

        # Verify a file was created in the temp directory
        saved_files = list(self.temp_data_dir.glob(f"{APP_NAME}_*.json"))
        self.assertEqual(len(saved_files), 1)

    @patch("asmith.send_matrix_message")
    async def test_load_command(self, mock_send_matrix_message):
        # Add a task, save it, then clear and load
        await self.todo_list_commands.add_task(self.room_id, self.sender, "Task to load")
        filename = await self.storage.save()
        self.storage.todo_lists = {} # Clear current state
        mock_send_matrix_message.reset_mock()

        await self.bot_management_commands.load_command(self.room_id, filename)

        mock_send_matrix_message.assert_called_once_with(
            self.mock_client,
            self.room_id,
            f"📂 Lists Loaded: Successfully loaded to-do lists from `{filename}`.",
            f"📂 Lists Loaded: Successfully loaded to-do lists from <code>{filename}</code>."
        )
        self.assertIn(self.room_id, self.storage.todo_lists)
        self.assertEqual(len(self.storage.todo_lists[self.room_id]), 1)
        self.assertEqual(self.storage.todo_lists[self.room_id][0].title, "Task to load")

    @patch("asmith.send_matrix_message")
    async def test_load_command_invalid_filename(self, mock_send_matrix_message):
        await self.bot_management_commands.load_command(self.room_id, "invalid_file.txt")

        mock_send_matrix_message.assert_called_once_with(
            self.mock_client,
            self.room_id,
            f"❌ Invalid Filename Format: Filename 'invalid_file.txt' does not match the expected format: `{APP_NAME}_<session_id>_<YYYY-MM-DD_HH-MM-SS>Z.json`",
            f"❌ Invalid Filename Format: Filename '<code>invalid_file.txt</code>' does not match the expected format: <code>{APP_NAME}_<session_id>_<YYYY-MM-DD_HH-MM-SS>Z.json</code>"
        )

    @patch("asmith.send_matrix_message")
    async def test_loadlast_command(self, mock_send_matrix_message):
        # Save multiple files to ensure loadlast picks the correct one
        await self.todo_list_commands.add_task(self.room_id, self.sender, "Old Task")
        await self.storage.save()
        self.storage.todo_lists = {} # Clear for next save
        await asyncio.sleep(0.01) # Ensure different timestamp for next file
        await self.todo_list_commands.add_task(self.room_id, self.sender, "New Task")
        latest_filename = await self.storage.save()
        self.storage.todo_lists = {} # Clear current state
        mock_send_matrix_message.reset_mock()

        await self.bot_management_commands.loadlast_command(self.room_id)

        mock_send_matrix_message.assert_called_once_with(
            self.mock_client,
            self.room_id,
            f"📂 Last List Loaded: Successfully loaded the most recent lists from `{latest_filename}`.",
            f"📂 Last List Loaded: Successfully loaded the most recent lists from <code>{latest_filename}</code>."
        )
        self.assertIn(self.room_id, self.storage.todo_lists)
        self.assertEqual(len(self.storage.todo_lists[self.room_id]), 1)
        self.assertEqual(self.storage.todo_lists[self.room_id][0].title, "New Task")

    @patch("asmith.send_matrix_message")
    async def test_list_files_command(self, mock_send_matrix_message):
        await self.todo_list_commands.add_task(self.room_id, self.sender, "Task 1")
        filename1 = await self.storage.save()
        self.storage.todo_lists = {}
        await asyncio.sleep(0.01)
        await self.todo_list_commands.add_task(self.room_id, self.sender, "Task 2")
        filename2 = await self.storage.save()
        mock_send_matrix_message.reset_mock()

        await self.bot_management_commands.list_files_command(self.room_id)

        expected_message_part = f"📄 Available Save Files:\n1. `{filename1}`\n2. `{filename2}`"
        expected_html_message_part = f"📄 Available Save Files:<br>1. <code>{filename1}</code><br>2. <code>{filename2}</code>"

        args, kwargs = mock_send_matrix_message.call_args
        self.assertIn(expected_message_part, args[1])
        self.assertIn(expected_html_message_part, kwargs['html_message'])


if __name__ == "__main__":
    unittest.main()
