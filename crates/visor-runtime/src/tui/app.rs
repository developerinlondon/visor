//! TUI application state machine.
//!
//! Manages view navigation, VM list, metrics, events, and keyboard input.
//! The [`App`] struct is the central state container for the terminal dashboard,
//! updated by polling the visor HTTP API and processing user input.

use crate::backend::{VmInfo, VmState};

/// Maximum number of events kept in the rolling buffer.
pub const MAX_EVENTS: usize = 100;

/// Active view in the TUI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum View {
    /// Main dashboard showing VMs, metrics, and events.
    Dashboard,
    /// Detail view for a single VM.
    VmDetail,
    /// Full-screen logs viewer for the selected VM.
    Logs,
}

/// Active dashboard pane (for Tab cycling).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Pane {
    /// VM list table.
    VmList,
    /// Metrics summary panel.
    Metrics,
    /// Events stream panel.
    Events,
}

/// User action derived from keyboard input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Action {
    /// Quit the application.
    Quit,
    /// Move selection up.
    Up,
    /// Move selection down.
    Down,
    /// Confirm selection / enter detail view.
    Enter,
    /// Go back to the previous view.
    Back,
    /// Switch active pane (Tab).
    SwitchPane,
    /// Toggle logs view.
    ToggleLogs,
    /// Open an interactive shell for the selected VM.
    OpenShell,
    /// Open the serial console for the selected VM.
    OpenConsole,
    /// Start a stopped or failed VM.
    Start,
    /// Request a graceful stop of the selected VM.
    Stop,
    /// Request a forceful kill of the selected VM.
    Kill,
    /// Request deletion of the selected VM.
    Delete,
    /// Confirm the pending destructive action.
    Confirm,
    /// Cancel the pending destructive action.
    Cancel,
    /// Open the create VM form.
    CreateNew,
}

/// A simplified event for display in the TUI.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct TuiEvent {
    /// Display timestamp (HH:MM:SS).
    pub timestamp: String,
    /// Event type (e.g. `"vm.created"`).
    pub event_type: String,
    /// ID of the affected VM.
    pub vm_id: String,
}

/// Captured stdout/stderr for a VM shown in the full-screen logs view.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct VmLogs {
    /// ID of the VM these logs belong to.
    pub vm_id: String,
    /// Optional human-friendly VM name.
    pub vm_name: Option<String>,
    /// Current VM state when the logs were fetched.
    pub vm_state: VmState,
    /// Captured standard output.
    pub stdout: String,
    /// Captured standard error.
    pub stderr: String,
}

impl VmLogs {
    /// Build a logs snapshot from a [`VmInfo`] record.
    #[must_use]
    pub fn from_vm(vm: &VmInfo) -> Self {
        Self {
            vm_id: vm.id.clone(),
            vm_name: vm.name.clone(),
            vm_state: vm.state,
            stdout: vm.stdout.clone().unwrap_or_default(),
            stderr: vm.stderr.clone().unwrap_or_default(),
        }
    }
}

/// Computed metrics summary for the dashboard.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct MetricsSummary {
    /// Total number of VMs.
    pub total_vms: usize,
    /// Number of VMs in `Running` state.
    pub running_vms: usize,
    /// Total allocated memory across all VMs (MiB).
    pub total_memory_mib: u64,
    /// Total virtual CPUs across all VMs.
    pub total_vcpus: u64,
    /// Number of warm (pre-booted) VMs in the pool.
    pub pool_warm_count: usize,
    /// Target number of warm VMs in the pool.
    pub pool_target: usize,
}

/// A destructive VM action awaiting user confirmation.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum PendingAction {
    /// Graceful stop (POST /{id}/stop).
    Stop {
        /// ID of the VM to stop.
        vm_id: String,
    },
    /// Forceful kill (POST /{id}/kill).
    Kill {
        /// ID of the VM to kill.
        vm_id: String,
    },
    /// Delete the VM (DELETE /{id}).
    Delete {
        /// ID of the VM to delete.
        vm_id: String,
    },
}

/// Preset image options for the create-VM form.
pub const IMAGE_PRESETS: &[&str] = &[
    "alpine:latest",
    "ubuntu:22.04",
    "debian:12",
    "nginx:latest",
    "postgres:16",
    "redis:latest",
];

/// Preset memory options `(label, value_mib)` for the create-VM form.
pub const MEMORY_PRESETS: &[(&str, u32)] = &[
    ("64 MiB", 64),
    ("128 MiB", 128),
    ("256 MiB", 256),
    ("512 MiB", 512),
];

/// Number of navigable rows in the create-VM form.
const FORM_ROW_COUNT: usize = 6;

/// Form state for creating a new VM from the TUI.
///
/// Rows: 0=Image(select), 1=Name(text), 2=Memory(select),
/// 3=vCPUs(text), 4=Command(text), 5=Buttons.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct CreateVmForm {
    /// Selected image preset index.
    pub image_preset: usize,
    /// Custom image text (active when `image_is_custom` is true).
    pub image_custom: String,
    /// Whether the image field is in custom-text mode.
    pub image_is_custom: bool,

    /// VM name (free text).
    pub name: String,

    /// Selected memory preset index.
    pub memory_preset: usize,
    /// Custom memory text in MiB (active when `memory_is_custom` is true).
    pub memory_custom: String,
    /// Whether the memory field is in custom-text mode.
    pub memory_is_custom: bool,

    /// Virtual CPU count (as editable text).
    pub vcpus: String,
    /// Command to run inside the VM.
    pub cmd: String,

    /// Currently selected row (0–5).
    pub selected_row: usize,
    /// Button selection within the button row (0=Create, 1=Cancel).
    pub button_index: usize,
    /// Character cursor position within the active text field.
    pub cursor_pos: usize,
    /// Validation error to display, if any.
    pub error: Option<String>,
}

impl CreateVmForm {
    /// Creates a new form with sensible defaults.
    #[must_use]
    pub fn new() -> Self {
        Self {
            image_preset: 0,
            image_custom: String::new(),
            image_is_custom: false,
            name: String::new(),
            memory_preset: 1, // 128 MiB
            memory_custom: String::new(),
            memory_is_custom: false,
            vcpus: "1".to_owned(),
            cmd: String::new(),
            selected_row: 0,
            button_index: 0,
            cursor_pos: 0,
            error: None,
        }
    }

    /// Whether the current row accepts text input.
    #[must_use]
    pub fn is_text_input_active(&self) -> bool {
        match self.selected_row {
            0 => self.image_is_custom,
            1 | 3 | 4 => true,
            2 => self.memory_is_custom,
            _ => false,
        }
    }

    /// Whether the current row is a select field in preset mode.
    #[must_use]
    pub fn is_preset_mode(&self) -> bool {
        match self.selected_row {
            0 => !self.image_is_custom,
            2 => !self.memory_is_custom,
            _ => false,
        }
    }

    /// Whether the current row is the button row.
    #[must_use]
    pub fn is_button_row(&self) -> bool {
        self.selected_row == FORM_ROW_COUNT - 1
    }

    /// Returns the active text field value for the current row.
    #[must_use]
    pub fn active_text(&self) -> &str {
        match self.selected_row {
            0 => &self.image_custom,
            1 => &self.name,
            2 => &self.memory_custom,
            3 => &self.vcpus,
            4 => &self.cmd,
            _ => "",
        }
    }

    /// Returns a mutable reference to the active text field.
    fn active_text_mut(&mut self) -> &mut String {
        match self.selected_row {
            0 => &mut self.image_custom,
            1 => &mut self.name,
            2 => &mut self.memory_custom,
            3 => &mut self.vcpus,
            4 | 5.. => &mut self.cmd,
        }
    }

    /// Inserts a character at the cursor position.
    pub fn insert_char(&mut self, c: char) {
        // On a select field in preset mode, switch to custom mode.
        match self.selected_row {
            0 if !self.image_is_custom => {
                self.image_is_custom = true;
                self.image_custom.clear();
                self.cursor_pos = 0;
            }
            2 if !self.memory_is_custom => {
                self.memory_is_custom = true;
                self.memory_custom.clear();
                self.cursor_pos = 0;
            }
            _ => {}
        }
        let pos = self.cursor_pos.min(self.active_text().len());
        self.cursor_pos = pos;
        self.active_text_mut().insert(pos, c);
        self.cursor_pos += c.len_utf8();
        self.error = None;
    }

    /// Deletes the character before the cursor.
    ///
    /// On a custom select field with empty text, reverts to preset mode.
    pub fn delete_char(&mut self) {
        if self.cursor_pos == 0 {
            // If custom mode with empty text, revert to preset.
            match self.selected_row {
                0 if self.image_is_custom && self.image_custom.is_empty() => {
                    self.image_is_custom = false;
                }
                2 if self.memory_is_custom && self.memory_custom.is_empty() => {
                    self.memory_is_custom = false;
                }
                _ => {}
            }
            return;
        }
        let cur = self.cursor_pos;
        let prev = self.active_text()[..cur]
            .char_indices()
            .next_back()
            .map_or(0, |(i, _)| i);
        self.active_text_mut().drain(prev..cur);
        self.cursor_pos = prev;
    }

    /// Moves the cursor one character to the left.
    pub fn move_cursor_left(&mut self) {
        if self.cursor_pos > 0 {
            let text = self.active_text();
            self.cursor_pos = text[..self.cursor_pos]
                .char_indices()
                .next_back()
                .map_or(0, |(i, _)| i);
        }
    }

    /// Moves the cursor one character to the right.
    pub fn move_cursor_right(&mut self) {
        let len = self.active_text().len();
        if self.cursor_pos < len {
            let text = self.active_text();
            self.cursor_pos = text[self.cursor_pos..]
                .char_indices()
                .nth(1)
                .map_or(len, |(i, _)| self.cursor_pos + i);
        }
    }

    /// Moves to the next row (Down / Tab).
    pub fn move_down(&mut self) {
        if self.selected_row < FORM_ROW_COUNT - 1 {
            self.selected_row += 1;
            self.sync_cursor();
        }
    }

    /// Moves to the previous row (Up / Shift+Tab).
    pub fn move_up(&mut self) {
        if self.selected_row > 0 {
            self.selected_row -= 1;
            self.sync_cursor();
        }
    }

    /// Cycles the select field or button one step to the right.
    pub fn cycle_right(&mut self) {
        match self.selected_row {
            0 => self.image_preset = (self.image_preset + 1) % IMAGE_PRESETS.len(),
            2 => self.memory_preset = (self.memory_preset + 1) % MEMORY_PRESETS.len(),
            5 => self.button_index = 1 - self.button_index,
            _ => {}
        }
    }

    /// Cycles the select field or button one step to the left.
    pub fn cycle_left(&mut self) {
        match self.selected_row {
            0 => {
                self.image_preset = if self.image_preset == 0 {
                    IMAGE_PRESETS.len() - 1
                } else {
                    self.image_preset - 1
                };
            }
            2 => {
                self.memory_preset = if self.memory_preset == 0 {
                    MEMORY_PRESETS.len() - 1
                } else {
                    self.memory_preset - 1
                };
            }
            5 => self.button_index = 1 - self.button_index,
            _ => {}
        }
    }

    /// Returns the resolved image value (preset or custom).
    #[must_use]
    pub fn image_value(&self) -> &str {
        if self.image_is_custom {
            &self.image_custom
        } else {
            IMAGE_PRESETS[self.image_preset]
        }
    }

    /// Returns the resolved memory in MiB, or an error message.
    ///
    /// # Errors
    ///
    /// Returns an error string if the custom memory value is not a valid number
    /// or is below the 64 MiB minimum.
    pub fn memory_mib(&self) -> Result<u32, &'static str> {
        if self.memory_is_custom {
            let v: u32 = self
                .memory_custom
                .trim()
                .parse()
                .map_err(|_| "Memory must be a number")?;
            if v < 64 {
                return Err("Memory must be at least 64 MiB");
            }
            Ok(v)
        } else {
            Ok(MEMORY_PRESETS[self.memory_preset].1)
        }
    }

    /// Sets cursor position to end of active text when switching rows.
    fn sync_cursor(&mut self) {
        if self.is_text_input_active() {
            self.cursor_pos = self.active_text().len();
        } else {
            self.cursor_pos = 0;
        }
    }

    /// Number of rows in the form.
    #[must_use]
    pub fn row_count() -> usize {
        FORM_ROW_COUNT
    }
}

impl Default for CreateVmForm {
    fn default() -> Self {
        Self::new()
    }
}

/// Central state container for the terminal dashboard.
///
/// Tracks which view is active, the selected VM, cached VM list, metrics,
/// events, and the daemon address for API polling.
#[non_exhaustive]
pub struct App {
    /// Current active view.
    view: View,
    /// Active dashboard pane.
    pane: Pane,
    /// Whether the user requested to quit.
    quit: bool,
    /// Index of the currently selected VM in the list.
    selected: usize,
    /// Cached VM list from the API.
    vms: Vec<VmInfo>,
    /// Rolling event buffer (newest last).
    events: Vec<TuiEvent>,
    /// Selected-VM logs shown in the full-screen logs view.
    logs: Option<VmLogs>,
    /// Daemon HTTP address.
    addr: String,
    /// Destructive action waiting for user confirmation.
    pending_action: Option<PendingAction>,
    /// Action that was confirmed and is ready for execution by the event loop.
    confirmed_action: Option<PendingAction>,
    /// Status message to display briefly after an action completes.
    status_message: Option<String>,
    /// Number of warm (pre-booted) VMs currently in the pool.
    pool_warm_count: usize,
    /// Target number of warm VMs the pool should maintain.
    pool_target: usize,
    /// Create-VM form state, when the overlay is open.
    create_form: Option<CreateVmForm>,
}

impl App {
    /// Creates a new `App` pointing at the given daemon address.
    #[must_use]
    pub fn new(addr: String) -> Self {
        Self {
            view: View::Dashboard,
            pane: Pane::VmList,
            quit: false,
            selected: 0,
            vms: Vec::new(),
            events: Vec::new(),
            logs: None,
            addr,
            pending_action: None,
            confirmed_action: None,
            status_message: None,
            pool_warm_count: 0,
            pool_target: 0,
            create_form: None,
        }
    }

    /// Returns the daemon HTTP address.
    #[must_use]
    pub fn addr(&self) -> &str {
        &self.addr
    }

    /// Returns the current active view.
    #[must_use]
    pub fn current_view(&self) -> View {
        self.view
    }

    /// Returns the active dashboard pane.
    #[must_use]
    pub fn active_pane(&self) -> Pane {
        self.pane
    }

    /// Returns whether the user requested to quit.
    #[must_use]
    pub fn should_quit(&self) -> bool {
        self.quit
    }

    /// Returns the index of the currently selected VM.
    #[must_use]
    pub fn selected_vm_index(&self) -> usize {
        self.selected
    }

    /// Returns the cached VM list.
    #[must_use]
    pub fn vms(&self) -> &[VmInfo] {
        &self.vms
    }

    /// Returns the event buffer.
    #[must_use]
    pub fn events(&self) -> &[TuiEvent] {
        &self.events
    }

    /// Returns the current full-screen VM logs snapshot, if any.
    #[must_use]
    pub fn logs(&self) -> Option<&VmLogs> {
        self.logs.as_ref()
    }

    /// Returns the selected logs VM ID, if the logs view is active.
    #[must_use]
    pub fn logs_vm_id(&self) -> Option<&str> {
        self.logs.as_ref().map(|logs| logs.vm_id.as_str())
    }

    /// Returns the currently selected VM, if any.
    #[must_use]
    pub fn selected_vm(&self) -> Option<&VmInfo> {
        self.vms.get(self.selected)
    }

    /// Replaces the VM list and clamps the selected index.
    pub fn set_vms(&mut self, vms: Vec<VmInfo>) {
        self.vms = vms;
        if self.vms.is_empty() {
            self.selected = 0;
        } else {
            self.selected = self.selected.min(self.vms.len() - 1);
        }
        if let Some(logs) = &mut self.logs {
            if let Some(vm) = self.vms.iter().find(|vm| vm.id == logs.vm_id) {
                *logs = VmLogs::from_vm(vm);
            }
        }
    }

    /// Appends an event, evicting the oldest if the buffer is full.
    pub fn push_event(&mut self, event: TuiEvent) {
        if self.events.len() >= MAX_EVENTS {
            self.events.remove(0);
        }
        self.events.push(event);
    }

    /// Replaces the current logs snapshot with fresh VM info.
    pub fn set_logs_from_vm(&mut self, vm: &VmInfo) {
        self.logs = Some(VmLogs::from_vm(vm));
    }

    /// Clears the current full-screen logs snapshot.
    pub fn clear_logs(&mut self) {
        self.logs = None;
    }

    /// Computes a metrics summary from the current VM list.
    #[must_use]
    pub fn compute_metrics(&self) -> MetricsSummary {
        let total_vms = self.vms.len();
        let running_vms = self
            .vms
            .iter()
            .filter(|v| v.state == VmState::Running)
            .count();
        let total_memory_mib = self.vms.iter().map(|v| u64::from(v.memory_mib)).sum();
        let total_vcpus = self.vms.iter().map(|v| u64::from(v.vcpus)).sum();
        MetricsSummary {
            total_vms,
            running_vms,
            total_memory_mib,
            total_vcpus,
            pool_warm_count: self.pool_warm_count,
            pool_target: self.pool_target,
        }
    }

    /// Processes a user action and updates state accordingly.
    pub fn handle_action(&mut self, action: Action) {
        match action {
            Action::Quit => self.quit = true,
            Action::Down => {
                if !self.vms.is_empty() && self.selected < self.vms.len() - 1 {
                    self.selected += 1;
                }
            }
            Action::Up => {
                if self.selected > 0 {
                    self.selected -= 1;
                }
            }
            Action::Enter => {
                if !self.vms.is_empty() {
                    self.view = View::VmDetail;
                }
            }
            Action::Back => {
                if self.view == View::Logs {
                    self.clear_logs();
                }
                self.view = View::Dashboard;
            }
            Action::SwitchPane => {
                self.pane = match self.pane {
                    Pane::VmList => Pane::Metrics,
                    Pane::Metrics => Pane::Events,
                    Pane::Events => Pane::VmList,
                };
            }
            Action::ToggleLogs => {
                self.view = if self.view == View::Logs {
                    self.clear_logs();
                    View::Dashboard
                } else if self.selected_vm().is_some() {
                    self.logs = self.selected_vm().map(VmLogs::from_vm);
                    View::Logs
                } else {
                    View::Dashboard
                };
            }
            Action::OpenShell | Action::OpenConsole | Action::Start => {}
            Action::Stop => {
                if let Some(vm) = self.selected_vm() {
                    self.pending_action = Some(PendingAction::Stop {
                        vm_id: vm.id.clone(),
                    });
                }
            }
            Action::Kill => {
                if let Some(vm) = self.selected_vm() {
                    self.pending_action = Some(PendingAction::Kill {
                        vm_id: vm.id.clone(),
                    });
                }
            }
            Action::Delete => {
                if let Some(vm) = self.selected_vm() {
                    self.pending_action = Some(PendingAction::Delete {
                        vm_id: vm.id.clone(),
                    });
                }
            }
            Action::CreateNew => {
                self.create_form = Some(CreateVmForm::new());
            }
            Action::Confirm => {
                self.confirmed_action = self.pending_action.take();
            }
            Action::Cancel => {
                self.pending_action = None;
            }
        }
    }

    /// Returns whether a destructive action is awaiting confirmation.
    #[must_use]
    pub fn has_pending_action(&self) -> bool {
        self.pending_action.is_some()
    }

    /// Returns a reference to the pending action, if any.
    #[must_use]
    pub fn pending_action(&self) -> Option<&PendingAction> {
        self.pending_action.as_ref()
    }

    /// Takes the confirmed action, returning it and clearing the field.
    ///
    /// Called by the event loop after `handle_action` to execute HTTP calls.
    #[must_use]
    pub fn take_confirmed_action(&mut self) -> Option<PendingAction> {
        self.confirmed_action.take()
    }

    /// Sets a status message to display briefly in the UI.
    pub fn set_status(&mut self, msg: String) {
        self.status_message = Some(msg);
    }

    /// Returns the current status message, if any.
    #[must_use]
    pub fn status_message(&self) -> Option<&str> {
        self.status_message.as_deref()
    }

    /// Sets the pool status (warm VM count and target).
    pub fn set_pool_status(&mut self, warm: usize, target: usize) {
        self.pool_warm_count = warm;
        self.pool_target = target;
    }

    /// Opens the create-VM form overlay.
    pub fn open_create_form(&mut self) {
        self.create_form = Some(CreateVmForm::new());
    }

    /// Closes the create-VM form overlay.
    pub fn close_create_form(&mut self) {
        self.create_form = None;
    }

    /// Returns whether the create-VM form is currently open.
    #[must_use]
    pub fn has_create_form(&self) -> bool {
        self.create_form.is_some()
    }

    /// Returns a reference to the create-VM form, if open.
    #[must_use]
    pub fn create_form(&self) -> Option<&CreateVmForm> {
        self.create_form.as_ref()
    }

    /// Returns a mutable reference to the create-VM form, if open.
    pub fn create_form_mut(&mut self) -> Option<&mut CreateVmForm> {
        self.create_form.as_mut()
    }

    /// Returns the number of warm VMs currently in the pool.
    #[must_use]
    pub fn pool_warm_count(&self) -> usize {
        self.pool_warm_count
    }

    /// Returns the target number of warm VMs the pool should maintain.
    #[must_use]
    pub fn pool_target(&self) -> usize {
        self.pool_target
    }
}

#[cfg(test)]
#[path = "app_test.rs"]
mod tests;
