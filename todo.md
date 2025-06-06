# Refactoring TODO for Production-Ready Matrix To-Do List Bot

This document outlines recommended refactoring tasks to elevate the Matrix To-Do List Bot codebase to a production-grade standard, based on its specific functionalities.

## 1. Code Structure and Organization

-   [ ] **Modularize Core Bot Functionality:**
    -   [ ] **`src/task_management/mod.rs`:**
        -   [ ] Define `Task` struct, `TaskEvent` enum, and related logic (from `main.rs` lines ~90-214).
        -   [ ] Implement `TodoList` struct and its methods for task operations (add, list, done, close, log, details, edit - from `main.rs` lines ~335-604).
    -   [ ] **`src/storage/mod.rs`:**
        -   [ ] Define `StorageData` and `StorageManager` structs and their methods for saving/loading task data (from `main.rs` lines ~218-320).
    -   [ ] **`src/bot_commands/mod.rs`:**
        -   [ ] Define the `BotCommand` trait (from `main.rs` lines ~324-332).
        -   [ ] Implement `BotManagement` struct and its methods for managing saved files and bot status (from `main.rs` lines ~635-830).
        -   [ ] Relocate `BotCore` (from `main.rs` lines ~1066-1267) here, focusing it on command parsing and dispatching to `TodoList` and `BotManagement`.
    -   [ ] **`src/matrix_integration/mod.rs`:**
        -   [ ] Consolidate Matrix client setup, login, session restoration (`restore_session`, `login_and_save_session` from `main.rs` lines ~1444-1602).
        -   [ ] House `handle_verification_events` (from `main.rs` lines ~861-1061) and other direct Matrix event handlers (like `on_stripped_state_member`).
        -   [ ] Include `ConnectionMonitor` logic (from `main.rs` lines ~1271-1407).
    -   [ ] **`src/config.rs`:** For loading and managing bot configuration (credentials, paths, etc.).
-   [ ] **Refactor `main.rs`:** Slim down to primarily handle:
    -   [ ] Argument parsing (`Args` struct).
    -   [ ] Initializing logging and configuration.
    -   [ ] Setting up the main `Client` and `BotCore` (or its replacement after modularization).
    -   [ ] Starting the Matrix sync loop and event handlers.
-   [ ] **Define Clear APIs Between Modules:** Ensure well-defined and minimal interfaces (e.g., `TodoList` should not directly know about `matrix_sdk::Client` for sending messages if possible, perhaps via a trait provided by `matrix_integration`).

## 2. Configuration Management

-   [ ] **Externalize All Configurations:** (As before, but re-emphasize for paths like `data_dir`, `session_file_path`).
-   [ ] **Secure Secrets Management:**
    -   [ ] **CRITICAL:** Remove hardcoded credentials (Memory `c1640e79-5d84-4982-8def-0c732df405e2`).
    -   [ ] **CRITICAL:** Securely manage the `store_passphrase` for the Matrix client's SQLite store (currently in `ClientConfig`/`PersistedSession`).

## 3. Error Handling

-   [ ] **Define Specific Error Types:**
    -   [ ] `TaskError` (e.g., `TaskNotFound`, `InvalidStatusTransition`).
    -   [ ] `StorageError` (e.g., `SaveFailed`, `LoadFailed`, `FileNotFound`).
    -   [ ] `CommandParseError` (e.g., `UnknownCommand`, `MissingArgument`).
    -   [ ] `MatrixInteractionError` (e.g. `MessageSendFailed`, `VerificationFailed`).
-   [ ] **Propagate Errors Clearly:** Ensure `Result` types are used consistently, especially in `TodoList`, `StorageManager`, and `BotCore` methods.

## 4. Logging

-   [ ] **Contextual Logging for Task Operations:** Log task ID, room ID, and user for all task modifications.
-   [ ] **Detailed Logging for Storage:** Log filenames and outcomes for save/load operations.
-   [ ] **Log Command Execution:** Log received commands and their outcomes (success/failure).

## 5. To-Do List Feature Refinements

-   [ ] **Task ID Management:** Review if the current `usize` ID per room is sufficient. Consider if tasks need unique IDs across all rooms or globally if the bot's scope might expand.
-   [ ] **`Task::internal_logs` vs `Task::logs`:** Clarify the purpose and ensure consistency. `internal_logs` seem to be a good audit trail.
-   [ ] **Command Parsing Robustness:** Improve parsing in `BotCore::process_command`. Consider using a more structured parsing library for commands if complexity grows.
-   [ ] **State Management for Tasks:** The `tasks: Arc<Mutex<HashMap<OwnedRoomId, Vec<Task>>>>` in `BotCore` is central. Ensure all accesses are correctly synchronized.
-   [ ] **User Feedback for Commands:** Ensure all commands provide clear success or failure messages to the user in the Matrix room.
-   [ ] **Review `BotManagement` commands:** (`!bot save`, `!bot load`, `!bot loadlast`, `!bot listfiles`, `!bot cleartasks`). Ensure they are intuitive and provide good feedback. Consider if `!bot cleartasks` should have a confirmation step.

## 6. Asynchronous Code and Concurrency

-   [ ] **Review `BotCore::process_command`:** This is a long async function. Ensure operations within it (especially those involving `await` and `Mutex` locks) are efficient and don't hold locks for too long.
-   [ ] **Concurrent Room Operations:** If the bot is in many active rooms, ensure task processing and storage operations for one room do not block others excessively.

## 7. Session Management (Matrix Client)

-   [ ] **Atomic Writes for `matrix_session.json`:** (As before, critical for preventing corruption).
-   [ ] **Error Handling in `restore_session` and `login_and_save_session`:** Ensure all I/O and SDK errors are handled gracefully with clear logs.

## 8. SAS Verification

-   [ ] **Thoroughly Test `handle_verification_events`:** (As before, this is complex and critical for E2EE).
-   [ ] **User Experience for Verification:** Ensure any necessary user interaction for verification (if any, beyond automatic) is clearly communicated.

## 9. Code Quality and Maintainability

-   [ ] **Refactor Large Functions:**
    -   [ ] `BotCore::process_command` (lines ~1082-1267) is very large; break it down by command type or into smaller helper functions.
    -   [ ] Individual command handlers in `TodoList` (e.g., `add_task`, `log_task`, `edit_task`) can also be reviewed for clarity and conciseness.
    -   [ ] `login_and_save_session` and `restore_session` are also quite large.
-   [ ] **Reduce `clone()` calls:** Investigate if some `clone()` calls can be avoided by using references, especially in loops or frequently called functions.
-   [ ] **Consistent Naming:** (As before).
-   [ ] **Documentation:** Document the new modules, structs (`Task`, `StorageManager`, etc.), and their public APIs.

## 10. Testing

-   [ ] **Unit Tests for `Task` struct methods:** (e.g., `add_log`, `set_status`).
-   [ ] **Unit Tests for `TodoList` logic:** (mocking Matrix interaction and storage) for adding, completing, logging, editing tasks.
-   [ ] **Unit Tests for `StorageManager`:** Test saving and loading various states of `StorageData`.
-   [ ] **Integration Tests for Command Processing:** Test the flow from `BotCore::process_command` to `TodoList` / `BotManagement` methods and expected Matrix replies (mocked).
-   [ ] **Test Data Persistence:** Verify that tasks are correctly saved and loaded across bot restarts.

## 11. Dependencies & Security

-   [ ] (As before - review, update, audit dependencies).
-   [ ] **Input Validation for Command Arguments:** Sanitize and validate all parts of user commands (task IDs, titles, log messages) to prevent unexpected behavior or injection issues (e.g., if task details are rendered as HTML).

## 12. Build and Deployment

-   [ ] (As before - release builds, consider Docker, document deployment).

This revised list should provide more specific guidance for refactoring your To-Do List Bot. Prioritize based on impact and available time.
