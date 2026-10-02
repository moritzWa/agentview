use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::control::RestorableSession;
use crate::domain::{
    AgentSession, Capability, LaunchTarget, Provider, SessionSnapshot, SessionState,
};
use crate::hidden::HiddenSessionRecord;
use crate::theme::ColorScheme;

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ViewMode {
    Status,
    Directory,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SelectionKey {
    Group(String),
    Session(String),
    ShowMore(String),
}

pub const SESSION_PAGE_SIZE: usize = 25;
pub const MODEL_PICKER_PAGE_SIZE: usize = 10;
pub const MIGRATION_PICKER_PAGE_SIZE: usize = 10;
pub const HIDDEN_PICKER_PAGE_SIZE: usize = 10;

/// Result rows the hidden-session picker can show in a terminal of `height`
/// rows. Rendering and PageUp/PageDown share it so a page is what fits.
pub fn hidden_picker_rows_for_height(height: u16) -> usize {
    HIDDEN_PICKER_PAGE_SIZE
        .min(usize::from(height.saturating_sub(7).max(1)))
        .max(1)
}
const MAX_SESSION_NAME_BYTES: usize = 240;
/// Slash commands the new-task composer runs instead of launching a session.
/// Keep in sync with `App::submit_new_session`.
const DASHBOARD_COMMANDS: &[&str] = &[
    "/help",
    "/harness",
    "/provider",
    "/model",
    "/shell",
    "/login",
    "/setup",
    "/cd",
    "/completed",
    "/filter",
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Overlay {
    None,
    Help,
    Peek,
    HarnessPicker,
    ModelPicker,
    HiddenPicker,
    DirectoryPicker,
    MigrationTargetPicker { session_id: String },
    Composer(ComposerMode),
    Confirm(ConfirmTarget),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CursorMovement {
    Left,
    Right,
    WordLeft,
    WordRight,
    Up,
    Down,
    LineStart,
    LineEnd,
}

/// Start of the shell-style word before `end`, skipping whitespace first.
fn previous_word_boundary(text: &str, end: usize) -> usize {
    let mut boundary = end;
    while let Some((index, character)) = text[..boundary].char_indices().next_back() {
        if !character.is_whitespace() {
            break;
        }
        boundary = index;
    }
    while let Some((index, character)) = text[..boundary].char_indices().next_back() {
        if character.is_whitespace() {
            break;
        }
        boundary = index;
    }
    boundary
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ComposerMode {
    NewSession,
    Rename {
        session_id: String,
    },
    MigrationName {
        session_id: String,
        target: Provider,
    },
    Filter,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConfirmTarget {
    Archive {
        id: String,
    },
    Hide {
        session_ids: Vec<String>,
    },
    Group {
        key: String,
        session_ids: Vec<String>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AppAction {
    None,
    Quit,
    Refresh,
    SetCompletedVisibility {
        include_completed: bool,
    },
    LoadModels {
        provider: Provider,
    },
    Authenticate {
        provider: Provider,
    },
    SetupProvider {
        provider: Provider,
    },
    SetupLaunchOption {
        provider: Provider,
        option: String,
    },
    Open {
        session_id: String,
    },
    Inspect {
        session_id: String,
    },
    Launch {
        provider: Provider,
        model: Option<String>,
        prompt: String,
        /// Directory picked in the directory view; `None` uses the launch cwd.
        cwd: Option<PathBuf>,
    },
    Reply {
        session_id: String,
        prompt: String,
    },
    ResolveApproval {
        session_id: String,
        accept: bool,
    },
    RespondInput {
        session_id: String,
        answer: String,
    },
    Rename {
        session_id: String,
        name: String,
    },
    Migrate {
        session_id: String,
        target: Provider,
        name: String,
    },
    Interrupt {
        session_id: String,
    },
    Archive {
        session_id: String,
    },
    Delete {
        session_ids: Vec<String>,
    },
    Hide {
        session_ids: Vec<String>,
    },
    /// Remember or forget a local pin. The row order is already updated.
    SetPin {
        session_id: String,
        pinned: bool,
    },
    /// Remember swapped sort keys after a manual move. The row order is
    /// already updated. Pinned rows store theirs as pin times.
    SetSortKeys {
        pinned: bool,
        keys: Vec<(String, u64)>,
    },
    /// Load the locally hidden registry into the restore picker.
    BrowseHidden,
    /// Remove one ID from the local hidden registry and reveal its row.
    Unhide {
        session_id: String,
    },
    /// Bring a persisted provider session back into the list, un-hiding it
    /// first when `hidden` is set, and reveal its row.
    Restore {
        session: RestorableSession,
        hidden: bool,
    },
}

#[derive(Clone, Debug)]
pub struct Group {
    pub key: String,
    pub label: String,
    pub sessions: Vec<usize>,
}

#[derive(Debug)]
pub struct App {
    pub snapshot: SessionSnapshot,
    pub includes_completed: bool,
    pub view_mode: ViewMode,
    pub selection: Option<SelectionKey>,
    pub overlay: Overlay,
    pub input: String,
    /// Byte offset of the composer cursor in `input`; `None` is the end.
    input_cursor: Option<usize>,
    pub filter: String,
    pub launch_targets: Vec<LaunchTarget>,
    pub launch_provider: Provider,
    pub launch_model: Option<String>,
    /// Directory chosen with `/cd` for the next new session. It wins over the
    /// directory view selection and is cleared once a session launches there.
    pub launch_directory_override: Option<PathBuf>,
    pub directory_candidates: Vec<PathBuf>,
    pub directory_filter: String,
    pub directory_selection: usize,
    /// Escape returns to the new-task draft when the picker was opened from it.
    directory_picker_from_composer: bool,
    pub yolo: bool,
    pub yolo_supported_providers: BTreeSet<Provider>,
    pub harness_selection: usize,
    pub available_models: Vec<String>,
    pub model_filter: String,
    pub model_selection: usize,
    pub migration_targets: Vec<Provider>,
    pub migration_selection: usize,
    pub pinned: BTreeMap<String, u64>,
    pub sort_keys: BTreeMap<String, u64>,
    pub hidden_candidates: Vec<HiddenSessionRecord>,
    pub hidden_filter: String,
    pub hidden_selection: usize,
    hidden_picker_rows: usize,
    /// Persisted sessions the restore picker offers besides hidden ones,
    /// keyed by row ID.
    pub restorable: BTreeMap<String, RestorableSession>,
    hidden_ids: BTreeSet<String>,
    pub restorable_loading: bool,
    pub models_loading: bool,
    pub models_provider: Option<Provider>,
    pub models_error: Option<String>,
    pub models_auth_available: bool,
    pub collapsed: BTreeSet<String>,
    visible_limits: BTreeMap<String, usize>,
    session_page_size: usize,
    group_cache: Vec<Group>,
    session_indices: HashMap<String, usize>,
    session_counts: BTreeMap<SessionState, usize>,
    provider_labels: Vec<String>,
    live_animation_visible: bool,
    #[cfg(test)]
    group_cache_rebuilds: usize,
    pub notice: Option<String>,
    pub details: BTreeMap<String, String>,
    pub refreshed_at: SystemTime,
    pub should_quit: bool,
    pub color_scheme: ColorScheme,
}

impl App {
    pub fn new(snapshot: SessionSnapshot) -> Self {
        Self::with_completed_visibility(snapshot, true)
    }

    pub fn with_completed_visibility(snapshot: SessionSnapshot, includes_completed: bool) -> Self {
        Self::with_launch_targets(
            snapshot,
            includes_completed,
            Provider::Claude,
            vec![LaunchTarget {
                provider: Provider::Claude,
                supports_model: true,
            }],
        )
    }

    pub fn with_launch_targets(
        snapshot: SessionSnapshot,
        includes_completed: bool,
        default_provider: Provider,
        launch_targets: Vec<LaunchTarget>,
    ) -> Self {
        let launch_provider = launch_targets
            .iter()
            .find(|target| target.provider == default_provider)
            .map(|target| target.provider.clone())
            .or_else(|| launch_targets.first().map(|target| target.provider.clone()))
            .unwrap_or(default_provider);
        let harness_selection = launch_targets
            .iter()
            .position(|target| target.provider == launch_provider)
            .unwrap_or(0);
        let mut app = Self {
            snapshot,
            includes_completed,
            view_mode: ViewMode::Status,
            selection: None,
            overlay: Overlay::None,
            input: String::new(),
            input_cursor: None,
            filter: String::new(),
            launch_targets,
            launch_provider,
            launch_model: None,
            launch_directory_override: None,
            directory_candidates: Vec::new(),
            directory_filter: String::new(),
            directory_selection: 0,
            directory_picker_from_composer: false,
            yolo: false,
            yolo_supported_providers: BTreeSet::new(),
            harness_selection,
            available_models: Vec::new(),
            model_filter: String::new(),
            model_selection: 0,
            migration_targets: Vec::new(),
            migration_selection: 0,
            pinned: BTreeMap::new(),
            sort_keys: BTreeMap::new(),
            hidden_candidates: Vec::new(),
            restorable: BTreeMap::new(),
            hidden_ids: BTreeSet::new(),
            restorable_loading: false,
            hidden_filter: String::new(),
            hidden_selection: 0,
            hidden_picker_rows: HIDDEN_PICKER_PAGE_SIZE,
            models_loading: false,
            models_provider: None,
            models_error: None,
            models_auth_available: false,
            collapsed: BTreeSet::new(),
            visible_limits: BTreeMap::new(),
            session_page_size: SESSION_PAGE_SIZE,
            group_cache: Vec::new(),
            session_indices: HashMap::new(),
            session_counts: BTreeMap::new(),
            provider_labels: Vec::new(),
            live_animation_visible: true,
            #[cfg(test)]
            group_cache_rebuilds: 0,
            notice: None,
            details: BTreeMap::new(),
            refreshed_at: SystemTime::now(),
            should_quit: false,
            color_scheme: ColorScheme::Dark,
        };
        app.rebuild_snapshot_cache();
        app.reconcile_selection();
        app
    }

    pub fn set_yolo(&mut self, enabled: bool, supported_providers: BTreeSet<Provider>) {
        self.yolo = enabled;
        self.yolo_supported_providers = supported_providers;
    }

    pub fn replace_snapshot(&mut self, snapshot: SessionSnapshot) {
        let previous_keys = self.ordered_keys();
        self.snapshot = snapshot;
        self.rebuild_snapshot_cache();
        self.refreshed_at = SystemTime::now();
        let previous_selection = self.selection.clone();
        self.reconcile_selection_near(&previous_keys);
        let selection_bound_overlay = matches!(
            self.overlay,
            Overlay::Peek
                | Overlay::MigrationTargetPicker { .. }
                | Overlay::Composer(ComposerMode::Rename { .. })
                | Overlay::Composer(ComposerMode::MigrationName { .. })
                | Overlay::Confirm(_)
        );
        if self.selection != previous_selection && selection_bound_overlay {
            self.overlay = Overlay::None;
            self.clear_input();
        }
    }

    pub fn groups(&self) -> &[Group] {
        &self.group_cache
    }

    pub fn selectable_keys(&self) -> Vec<SelectionKey> {
        let mut keys = Vec::new();
        for group in self.groups() {
            keys.push(SelectionKey::Group(group.key.clone()));
            if !self.collapsed.contains(&group.key) {
                let visible = self.visible_session_count(group);
                keys.extend(
                    group.sessions.iter().take(visible).map(|index| {
                        SelectionKey::Session(self.snapshot.sessions[*index].id.clone())
                    }),
                );
                if visible < group.sessions.len() {
                    keys.push(SelectionKey::ShowMore(group.key.clone()));
                }
            }
        }
        keys
    }

    /// Every row in list order, including sessions beyond a group's current
    /// page. Used to find the row that slides up into view after a removal,
    /// which the paged `selectable_keys` would skip.
    fn ordered_keys(&self) -> Vec<SelectionKey> {
        let mut keys = Vec::new();
        for group in self.groups() {
            keys.push(SelectionKey::Group(group.key.clone()));
            keys.extend(
                group
                    .sessions
                    .iter()
                    .map(|index| SelectionKey::Session(self.snapshot.sessions[*index].id.clone())),
            );
            if self.visible_session_count(group) < group.sessions.len() {
                keys.push(SelectionKey::ShowMore(group.key.clone()));
            }
        }
        keys
    }

    pub fn visible_session_count(&self, group: &Group) -> usize {
        self.visible_limits
            .get(&group.key)
            .copied()
            .unwrap_or(self.session_page_size)
            .min(group.sessions.len())
    }

    pub fn session_page_size(&self) -> usize {
        self.session_page_size
    }

    pub fn set_session_page_size(&mut self, page_size: usize) {
        let page_size = page_size.clamp(1, SESSION_PAGE_SIZE);
        if self.session_page_size == page_size {
            return;
        }
        self.session_page_size = page_size;
        self.visible_limits.clear();
        self.reconcile_selection();
    }

    pub fn hidden_session_count(&self, group: &Group) -> usize {
        group
            .sessions
            .len()
            .saturating_sub(self.visible_session_count(group))
    }

    pub fn session_count(&self, state: SessionState) -> usize {
        self.session_counts.get(&state).copied().unwrap_or(0)
    }

    /// Advance the low-frequency live-session marker. The snapshot and group
    /// indexes remain untouched, so animation never rebuilds a large queue.
    pub fn advance_live_animation(&mut self) -> bool {
        if self.session_count(SessionState::Working) == 0 {
            let changed = !self.live_animation_visible;
            self.live_animation_visible = true;
            return changed;
        }
        self.live_animation_visible = !self.live_animation_visible;
        true
    }

    pub fn live_animation_visible(&self) -> bool {
        self.live_animation_visible
    }

    pub fn provider_labels(&self) -> &[String] {
        &self.provider_labels
    }

    pub fn select_next(&mut self) {
        self.move_selection(1);
    }

    pub fn select_previous(&mut self) {
        self.move_selection(-1);
    }

    fn move_selection(&mut self, delta: isize) {
        let keys = self.selectable_keys();
        if keys.is_empty() {
            self.selection = None;
            return;
        }
        let current = self
            .selection
            .as_ref()
            .and_then(|selected| keys.iter().position(|key| key == selected));
        let next = match current {
            Some(index) => (index as isize + delta).rem_euclid(keys.len() as isize) as usize,
            None if delta < 0 => keys.len() - 1,
            None => 0,
        };
        self.selection = Some(keys[next].clone());
        self.notice = None;
    }

    pub fn selected_session(&self) -> Option<&AgentSession> {
        let SelectionKey::Session(id) = self.selection.as_ref()? else {
            return None;
        };
        let index = *self.session_indices.get(id)?;
        self.snapshot
            .sessions
            .get(index)
            .filter(|session| &session.id == id)
    }

    pub fn session_by_id(&self, session_id: &str) -> Option<&AgentSession> {
        let index = *self.session_indices.get(session_id)?;
        self.snapshot
            .sessions
            .get(index)
            .filter(|session| session.id == session_id)
    }

    /// Whether the latest snapshot contains `session_id`, even when the
    /// current filter keeps it out of every group.
    pub fn snapshot_contains(&self, session_id: &str) -> bool {
        self.session_indices.contains_key(session_id)
    }

    pub fn select_and_reveal_session(&mut self, session_id: &str) -> bool {
        let Some(index) = self.session_indices.get(session_id).copied() else {
            return false;
        };
        let Some((group_key, position, group_len)) = self.groups().iter().find_map(|group| {
            group
                .sessions
                .iter()
                .position(|candidate| *candidate == index)
                .map(|position| (group.key.clone(), position, group.sessions.len()))
        }) else {
            return false;
        };
        self.collapsed.remove(&group_key);
        let required = position.saturating_add(1).min(group_len);
        let current = self
            .visible_limits
            .get(&group_key)
            .copied()
            .unwrap_or(self.session_page_size);
        if required > current {
            self.visible_limits.insert(group_key, required);
        }
        self.selection = Some(SelectionKey::Session(session_id.to_owned()));
        true
    }

    pub fn selected_group(&self) -> Option<Group> {
        let SelectionKey::Group(key) = self.selection.as_ref()? else {
            return None;
        };
        self.groups()
            .iter()
            .find(|group| &group.key == key)
            .cloned()
    }

    /// The project directory selected in the directory view, which the header
    /// shows and new sessions start in.
    pub fn launch_directory(&self) -> Option<PathBuf> {
        if let Some(directory) = &self.launch_directory_override {
            return Some(directory.clone());
        }
        if self.view_mode != ViewMode::Directory {
            return None;
        }
        self.selected_session()
            .map(|session| project_group_path(&session.cwd))
            .or_else(|| {
                self.selected_group().and_then(|group| {
                    group
                        .sessions
                        .first()
                        .map(|index| project_group_path(&self.snapshot.sessions[*index].cwd))
                })
            })
    }

    pub fn activate(&mut self) -> AppAction {
        match self.overlay.clone() {
            Overlay::Help => {
                self.overlay = Overlay::None;
                AppAction::None
            }
            Overlay::Peek if self.input.trim().is_empty() => self.request_open(),
            Overlay::Peek => {
                let Some(session) = self.selected_session() else {
                    return AppAction::None;
                };
                if session.capabilities.contains(&Capability::Respond) {
                    let action = AppAction::RespondInput {
                        session_id: session.id.clone(),
                        answer: self.input.trim().to_owned(),
                    };
                    self.clear_input();
                    return action;
                }
                if !session.capabilities.contains(&Capability::Reply) {
                    let name = session.name.clone();
                    self.set_notice(format!(
                        "{name} is read-only; reply authority was not granted"
                    ));
                    self.clear_input();
                    return AppAction::None;
                }
                let action = AppAction::Reply {
                    session_id: session.id.clone(),
                    prompt: self.input.trim().to_owned(),
                };
                self.clear_input();
                action
            }
            Overlay::HarnessPicker => {
                self.confirm_harness_selection();
                AppAction::None
            }
            Overlay::ModelPicker => {
                if self.models_error.is_some()
                    && self.models_auth_available
                    && !self.has_valid_custom_model_input()
                {
                    AppAction::Authenticate {
                        provider: self.launch_provider.clone(),
                    }
                } else {
                    self.confirm_model_selection()
                }
            }
            Overlay::MigrationTargetPicker { .. } => {
                self.confirm_migration_target();
                AppAction::None
            }
            Overlay::HiddenPicker => self.confirm_hidden_selection(),
            Overlay::DirectoryPicker => {
                self.confirm_directory_selection();
                AppAction::None
            }
            Overlay::Composer(mode) => self.submit_composer(mode),
            Overlay::Confirm(target) => self.confirm(target),
            Overlay::None => match self.selection.clone() {
                Some(SelectionKey::Group(key)) => {
                    if !self.collapsed.remove(&key) {
                        self.collapsed.insert(key);
                    }
                    AppAction::None
                }
                Some(SelectionKey::Session(_)) => self.request_open(),
                Some(SelectionKey::ShowMore(key)) => {
                    self.show_more(&key);
                    AppAction::None
                }
                None => AppAction::None,
            },
        }
    }

    pub fn escape(&mut self) -> AppAction {
        match self.overlay.clone() {
            Overlay::None => {
                self.should_quit = true;
                AppAction::Quit
            }
            Overlay::HarnessPicker => {
                self.overlay = Overlay::Composer(ComposerMode::NewSession);
                self.notice = None;
                AppAction::None
            }
            Overlay::ModelPicker => {
                self.overlay = Overlay::Composer(ComposerMode::NewSession);
                self.model_filter.clear();
                self.notice = None;
                AppAction::None
            }
            Overlay::Composer(ComposerMode::MigrationName { session_id, .. }) => {
                self.overlay = Overlay::MigrationTargetPicker { session_id };
                self.clear_input();
                self.notice = None;
                AppAction::None
            }
            Overlay::MigrationTargetPicker { .. } => {
                self.overlay = Overlay::None;
                self.clear_input();
                self.notice = None;
                AppAction::None
            }
            Overlay::HiddenPicker => {
                self.overlay = Overlay::None;
                self.hidden_filter.clear();
                self.hidden_candidates.clear();
                self.restorable.clear();
                self.restorable_loading = false;
                self.notice = None;
                AppAction::None
            }
            Overlay::DirectoryPicker => {
                self.close_directory_picker();
                AppAction::None
            }
            _ => {
                self.overlay = Overlay::None;
                self.clear_input();
                AppAction::None
            }
        }
    }

    pub fn toggle_help(&mut self) {
        self.notice = None;
        self.overlay = if self.overlay == Overlay::Help {
            Overlay::None
        } else {
            Overlay::Help
        };
    }

    pub fn toggle_view(&mut self) {
        self.notice = None;
        self.set_view_mode(match self.view_mode {
            ViewMode::Status => ViewMode::Directory,
            ViewMode::Directory => ViewMode::Status,
        });
    }

    pub fn set_view_mode(&mut self, view_mode: ViewMode) {
        self.view_mode = view_mode;
        self.collapsed.clear();
        self.visible_limits.clear();
        self.rebuild_group_cache();
        self.reconcile_selection();
    }

    pub fn toggle_peek(&mut self) {
        let Some(session) = self.selected_session() else {
            return;
        };
        if !session.capabilities.contains(&Capability::Inspect) {
            let name = session.name.clone();
            self.set_notice(format!(
                "{name} is observe/open-only; transcript inspection was not granted"
            ));
            return;
        }
        self.overlay = if self.overlay == Overlay::Peek {
            self.clear_input();
            Overlay::None
        } else {
            Overlay::Peek
        };
        self.notice = None;
    }

    pub fn start_new_session(&mut self, first_character: Option<char>) {
        self.notice = None;
        self.clear_input();
        if let Some(character) = first_character {
            self.input.push(character);
        }
        self.overlay = Overlay::Composer(ComposerMode::NewSession);
    }

    pub fn open_harness_picker(&mut self) {
        if self.launch_targets.is_empty() {
            self.set_notice("no launch-capable harness is configured");
            return;
        }
        self.harness_selection = self
            .launch_targets
            .iter()
            .position(|target| target.provider == self.launch_provider)
            .unwrap_or(0);
        self.notice = None;
        self.overlay = Overlay::HarnessPicker;
    }

    pub fn move_harness_selection(&mut self, delta: isize) {
        if self.launch_targets.is_empty() {
            return;
        }
        self.harness_selection = (self.harness_selection as isize + delta)
            .rem_euclid(self.launch_targets.len() as isize)
            as usize;
    }

    pub fn choose_harness_number(&mut self, number: usize) {
        let Some(index) = number.checked_sub(1) else {
            return;
        };
        if index < self.launch_targets.len() {
            self.harness_selection = index;
            self.confirm_harness_selection();
        }
    }

    pub fn confirm_harness_selection(&mut self) {
        let Some(target) = self.launch_targets.get(self.harness_selection) else {
            self.overlay = Overlay::Composer(ComposerMode::NewSession);
            return;
        };
        if self.launch_provider != target.provider {
            self.launch_provider = target.provider.clone();
            self.launch_model = None;
        }
        self.notice = None;
        self.overlay = Overlay::Composer(ComposerMode::NewSession);
    }

    pub fn launch_target(&self) -> Option<&LaunchTarget> {
        self.launch_targets
            .iter()
            .find(|target| target.provider == self.launch_provider)
    }

    pub fn open_model_picker(&mut self) -> AppAction {
        if !self
            .launch_target()
            .is_some_and(|target| target.supports_model)
        {
            self.set_notice(format!(
                "{} does not expose model selection",
                self.launch_provider.label()
            ));
            return AppAction::None;
        }
        if self.models_provider.as_ref() != Some(&self.launch_provider) {
            self.available_models.clear();
        }
        self.model_filter.clear();
        self.model_selection = self
            .model_choices()
            .iter()
            .position(|choice| *choice == self.launch_model.as_deref())
            .unwrap_or(0);
        self.models_loading = true;
        self.models_provider = Some(self.launch_provider.clone());
        self.models_error = None;
        self.models_auth_available = false;
        self.notice = None;
        self.overlay = Overlay::ModelPicker;
        AppAction::LoadModels {
            provider: self.launch_provider.clone(),
        }
    }

    pub fn set_available_models(
        &mut self,
        provider: Provider,
        result: Result<Vec<String>, String>,
    ) {
        if self.models_provider.as_ref() != Some(&provider) {
            return;
        }
        self.models_loading = false;
        match result {
            Ok(models) => {
                let mut seen = BTreeSet::new();
                self.available_models = models
                    .into_iter()
                    .filter(|model| valid_model_name(model))
                    .filter(|model| seen.insert(model.clone()))
                    .collect();
                self.model_selection = self
                    .model_choices()
                    .iter()
                    .position(|choice| *choice == self.launch_model.as_deref())
                    .unwrap_or(0);
                self.reconcile_model_selection();
                self.models_error = None;
                self.notice = None;
            }
            Err(error) => {
                self.available_models.clear();
                self.model_selection = 0;
                self.models_error = Some(error);
                // The picker renders a bounded, wrapped recovery message. A
                // duplicate one-line notice would be clipped and would hide
                // its contextual retry/setup keys.
                self.notice = None;
            }
        }
    }

    pub fn set_models_auth_available(&mut self, provider: &Provider, available: bool) {
        if self.models_provider.as_ref() == Some(provider) {
            self.models_auth_available = available;
        }
    }

    pub fn retry_model_load(&mut self, provider: &Provider) {
        if self.models_provider.as_ref() == Some(provider) {
            self.models_loading = true;
            self.models_error = None;
            self.available_models.clear();
            self.model_selection = 0;
        }
    }

    /// Preserve an attempted task and turn an authentication failure into an
    /// explicit modal action. A plain dashboard notice is not actionable:
    /// Enter would otherwise open whichever session row remained selected.
    pub fn require_authentication(
        &mut self,
        provider: Provider,
        model: Option<String>,
        prompt: String,
        error: String,
    ) {
        self.launch_provider = provider.clone();
        self.launch_model = model;
        self.set_input(prompt);
        self.model_filter.clear();
        self.available_models.clear();
        self.model_selection = 0;
        self.models_loading = false;
        self.models_provider = Some(provider);
        self.models_error = Some(error);
        self.models_auth_available = true;
        self.notice = None;
        self.overlay = Overlay::ModelPicker;
    }

    pub fn model_choices(&self) -> Vec<Option<&str>> {
        let needle = self.model_filter.to_ascii_lowercase();
        let mut choices = Vec::new();
        if self.models_error.is_none() && (needle.is_empty() || "default".contains(&needle)) {
            choices.push(None);
        }
        choices.extend(
            self.available_models
                .iter()
                .filter(|model| model.to_ascii_lowercase().contains(&needle))
                .map(|model| Some(model.as_str())),
        );
        choices
    }

    pub fn has_valid_custom_model_input(&self) -> bool {
        // Antigravity validates every --model value against the same catalog
        // that just failed. Treating filter text as an exact ID in that state
        // turns a recovery Enter into another guaranteed launch failure.
        (self.models_error.is_some() && self.launch_provider != Provider::Antigravity
            || self.launch_provider == Provider::QwenCode
                && self.models_error.is_none()
                && self.available_models.is_empty())
            && valid_model_name(&self.model_filter)
    }

    pub fn move_model_selection(&mut self, delta: isize) {
        let len = self.model_choices().len();
        if len == 0 {
            self.model_selection = 0;
            return;
        }
        self.model_selection =
            (self.model_selection as isize + delta).rem_euclid(len as isize) as usize;
    }

    pub fn move_model_page(&mut self, delta: isize) {
        let len = self.model_choices().len();
        if len == 0 {
            self.model_selection = 0;
            return;
        }
        self.model_selection = (self.model_selection as isize
            + delta * MODEL_PICKER_PAGE_SIZE as isize)
            .clamp(0, len.saturating_sub(1) as isize) as usize;
    }

    pub fn confirm_model_selection(&mut self) -> AppAction {
        if self.has_valid_custom_model_input() {
            self.launch_model = Some(self.model_filter.clone());
            self.model_filter.clear();
            self.notice = None;
            self.overlay = Overlay::Composer(ComposerMode::NewSession);
            return AppAction::None;
        }
        let selection = self
            .model_choices()
            .get(self.model_selection)
            .copied()
            .flatten()
            .map(ToOwned::to_owned);
        if self.model_choices().is_empty() {
            return AppAction::None;
        }
        if self.launch_provider == Provider::Terminal {
            if let Some(option) = selection
                .as_deref()
                .filter(|value| crate::adapters::is_shell_install_choice(value))
            {
                self.model_filter.clear();
                self.notice = None;
                return AppAction::SetupLaunchOption {
                    provider: Provider::Terminal,
                    option: option.to_owned(),
                };
            }
        }
        self.launch_model = selection;
        self.model_filter.clear();
        self.notice = None;
        self.overlay = Overlay::Composer(ComposerMode::NewSession);
        AppAction::None
    }

    pub fn set_completed_visibility(&mut self, include_completed: bool) {
        self.includes_completed = include_completed;
        self.visible_limits.clear();
        self.notice = Some(if include_completed {
            "showing completed sessions".into()
        } else {
            "completed sessions hidden".into()
        });
        self.reconcile_selection();
    }

    pub fn reveal_completed_after_launch(&mut self) {
        self.includes_completed = true;
        self.visible_limits.clear();
        self.notice =
            Some("task finished before the first refresh; showing completed sessions".into());
        self.reconcile_selection();
    }

    pub fn start_filter(&mut self) {
        self.notice = None;
        self.set_input(self.filter.clone());
        self.overlay = Overlay::Composer(ComposerMode::Filter);
    }

    /// Replace the session filter and rebuild the grouping cache once. The
    /// cache keeps subsequent draws and arrow-key navigation independent of
    /// the full provider-history size.
    pub fn set_filter(&mut self, filter: impl Into<String>) {
        self.filter = filter.into();
        self.visible_limits.clear();
        self.rebuild_group_cache();
        self.reconcile_selection();
    }

    pub fn start_rename(&mut self) {
        let Some(session) = self.selected_session() else {
            return;
        };
        let session_id = session.id.clone();
        self.notice = None;
        self.clear_input();
        self.overlay = Overlay::Composer(ComposerMode::Rename { session_id });
    }

    pub fn start_migration(&mut self) {
        let Some(session) = self.selected_session() else {
            self.set_notice("select a session to migrate");
            return;
        };
        if crate::migration::provider_format(&session.provider).is_none() {
            let provider = session.provider.label().to_owned();
            self.set_notice(format!("{provider} sessions cannot be migrated"));
            return;
        }
        if session.runtime != crate::domain::Runtime::Host {
            self.set_notice("session migration currently supports host sessions only");
            return;
        }
        let session_id = session.id.clone();
        self.migration_targets = crate::migration::migration_targets(&session.provider);
        self.migration_selection = 0;
        self.clear_input();
        self.notice = None;
        self.overlay = Overlay::MigrationTargetPicker { session_id };
    }

    pub fn move_migration_selection(&mut self, delta: isize) {
        if self.migration_targets.is_empty() {
            return;
        }
        self.migration_selection = (self.migration_selection as isize + delta)
            .rem_euclid(self.migration_targets.len() as isize)
            as usize;
    }

    pub fn move_migration_page(&mut self, delta: isize) {
        if self.migration_targets.is_empty() {
            return;
        }
        self.migration_selection = (self.migration_selection as isize
            + delta * MIGRATION_PICKER_PAGE_SIZE as isize)
            .clamp(0, self.migration_targets.len().saturating_sub(1) as isize)
            as usize;
    }

    pub fn confirm_migration_target(&mut self) {
        let Overlay::MigrationTargetPicker { session_id } = self.overlay.clone() else {
            return;
        };
        let Some(target) = self
            .migration_targets
            .get(self.migration_selection)
            .cloned()
        else {
            return;
        };
        let Some(session) = self
            .snapshot
            .sessions
            .iter()
            .find(|session| session.id == session_id)
        else {
            self.overlay = Overlay::None;
            self.set_notice("the selected session disappeared during refresh");
            return;
        };
        self.set_input(default_migration_name(&session.name, target.label()));
        self.notice = None;
        self.overlay = Overlay::Composer(ComposerMode::MigrationName { session_id, target });
    }

    pub fn start_confirm(&mut self) -> AppAction {
        if let Some(session) = self.selected_session() {
            let running = is_active_session_state(session.state);
            let id = session.id.clone();
            let action = if session.capabilities.contains(&Capability::Interrupt) {
                AppAction::Interrupt { session_id: id }
            } else if !running && session.capabilities.contains(&Capability::Delete) {
                AppAction::Delete {
                    session_ids: vec![id],
                }
            } else if !running {
                AppAction::Hide {
                    session_ids: vec![id],
                }
            } else {
                self.overlay = Overlay::Confirm(ConfirmTarget::Hide {
                    session_ids: vec![id],
                });
                self.notice = None;
                return AppAction::None;
            };
            self.overlay = Overlay::None;
            self.notice = None;
            return action;
        } else if let Some(group) = self.selected_group() {
            if group
                .sessions
                .iter()
                .any(|index| is_active_session_state(self.snapshot.sessions[*index].state))
            {
                self.set_notice("bulk stop is unavailable; select one running session");
                return AppAction::None;
            }
            let undeletable = group
                .sessions
                .iter()
                .filter(|index| {
                    !self.snapshot.sessions[**index]
                        .capabilities
                        .contains(&Capability::Delete)
                })
                .map(|index| self.snapshot.sessions[*index].id.clone())
                .collect::<Vec<_>>();
            if !undeletable.is_empty() {
                self.overlay = Overlay::Confirm(ConfirmTarget::Hide {
                    session_ids: undeletable,
                });
                self.notice = None;
                return AppAction::None;
            }
            self.overlay = Overlay::Confirm(ConfirmTarget::Group {
                key: group.key,
                session_ids: group
                    .sessions
                    .iter()
                    .map(|index| self.snapshot.sessions[*index].id.clone())
                    .collect(),
            });
            self.notice = None;
        }
        AppAction::None
    }

    pub fn start_archive_confirm(&mut self) {
        let Some(session) = self.selected_session() else {
            return;
        };
        if is_active_session_state(session.state) {
            let name = session.name.clone();
            self.set_notice(format!("{name} must be idle before it can be archived"));
            return;
        }
        if !session.capabilities.contains(&Capability::Archive) {
            let name = session.name.clone();
            self.set_notice(format!(
                "{name} is observe-only; Archive authority was not granted"
            ));
            return;
        }
        self.overlay = Overlay::Confirm(ConfirmTarget::Archive {
            id: session.id.clone(),
        });
        self.notice = None;
    }

    pub fn resolve_approval(&mut self, accept: bool) -> AppAction {
        if self.overlay != Overlay::Peek {
            return AppAction::None;
        }
        let Some(session) = self.selected_session() else {
            return AppAction::None;
        };
        let required = if accept {
            Capability::Approve
        } else {
            Capability::Decline
        };
        if !session.capabilities.contains(&required) {
            return AppAction::None;
        }
        AppAction::ResolveApproval {
            session_id: session.id.clone(),
            accept,
        }
    }

    pub fn push_input(&mut self, character: char) {
        let picker_filter = matches!(
            self.overlay,
            Overlay::ModelPicker | Overlay::HiddenPicker | Overlay::DirectoryPicker
        );
        if picker_filter && matches!(character, '\n' | '\r') {
            // Search fields are single-line; ctrl+j must not insert a newline
            // that renders as an invisible, unmatchable filter character.
            return;
        }
        if self.overlay == Overlay::ModelPicker {
            self.model_filter.push(character);
            self.reconcile_model_selection();
            return;
        }
        if self.overlay == Overlay::HiddenPicker {
            self.hidden_filter.push(character);
            self.reconcile_hidden_selection();
            return;
        }
        if self.overlay == Overlay::DirectoryPicker {
            self.directory_filter.push(character);
            self.directory_selection = 0;
            return;
        }
        let peek_is_writable = self.overlay == Overlay::Peek
            && self.selected_session().is_some_and(|session| {
                session.capabilities.contains(&Capability::Reply)
                    || session.capabilities.contains(&Capability::Respond)
            });
        if peek_is_writable || matches!(self.overlay, Overlay::Composer(_)) {
            let cursor = self.input_cursor();
            self.input.insert(cursor, character);
            if self.input_cursor.is_some() {
                self.input_cursor = Some(cursor + character.len_utf8());
            }
        }
    }

    fn clear_input(&mut self) {
        self.input.clear();
        self.input_cursor = None;
    }

    fn set_input(&mut self, text: String) {
        self.input = text;
        self.input_cursor = None;
    }

    /// Where typing lands in `input`. Only composers have a movable cursor;
    /// peek replies and anything else always edit at the end.
    pub fn input_cursor(&self) -> usize {
        match self.input_cursor {
            Some(cursor)
                if matches!(self.overlay, Overlay::Composer(_))
                    && self.input.is_char_boundary(cursor) =>
            {
                cursor
            }
            _ => self.input.len(),
        }
    }

    fn place_input_cursor(&mut self, cursor: usize) {
        self.input_cursor = (cursor < self.input.len()).then_some(cursor);
    }

    /// Whether Shift/Alt/Ctrl+Enter add a line instead of submitting: true in
    /// the new-task composer and while writing a peek reply.
    pub fn draft_accepts_line_breaks(&self) -> bool {
        self.overlay == Overlay::Composer(ComposerMode::NewSession)
            || (self.overlay == Overlay::Peek && !self.input.is_empty())
    }

    /// Whether arrow and Home/End keys move the cursor in the draft instead
    /// of acting on the dashboard.
    pub fn input_cursor_movable(&self) -> bool {
        matches!(self.overlay, Overlay::Composer(_))
    }

    pub fn move_input_cursor(&mut self, movement: CursorMovement) {
        if !self.input_cursor_movable() {
            return;
        }
        let cursor = self.input_cursor();
        let line_start = self.input[..cursor]
            .rfind('\n')
            .map_or(0, |index| index + 1);
        let line_end = self.input[cursor..]
            .find('\n')
            .map_or(self.input.len(), |index| cursor + index);
        let target = match movement {
            CursorMovement::Left => self.input[..cursor]
                .char_indices()
                .next_back()
                .map_or(0, |(index, _)| index),
            CursorMovement::Right => self.input[cursor..]
                .chars()
                .next()
                .map_or(cursor, |character| cursor + character.len_utf8()),
            CursorMovement::WordLeft => previous_word_boundary(&self.input, cursor),
            CursorMovement::WordRight => {
                let rest = &self.input[cursor..];
                let skipped = rest.len() - rest.trim_start().len();
                let word = rest[skipped..]
                    .find(char::is_whitespace)
                    .unwrap_or(rest.len() - skipped);
                cursor + skipped + word
            }
            CursorMovement::LineStart => line_start,
            CursorMovement::LineEnd => line_end,
            CursorMovement::Up | CursorMovement::Down => {
                let column = self.input[line_start..cursor].chars().count();
                let (start, end) = if movement == CursorMovement::Up {
                    if line_start == 0 {
                        self.place_input_cursor(0);
                        return;
                    }
                    let end = line_start - 1;
                    (
                        self.input[..end].rfind('\n').map_or(0, |index| index + 1),
                        end,
                    )
                } else {
                    if line_end == self.input.len() {
                        self.place_input_cursor(line_end);
                        return;
                    }
                    let start = line_end + 1;
                    let end = self.input[start..]
                        .find('\n')
                        .map_or(self.input.len(), |index| start + index);
                    (start, end)
                };
                self.input[start..end]
                    .char_indices()
                    .nth(column)
                    .map_or(end, |(index, _)| start + index)
            }
        };
        self.place_input_cursor(target);
    }

    /// Delete the character after the cursor (the Delete key).
    pub fn delete_next_input(&mut self) {
        if !self.input_cursor_movable() {
            return;
        }
        let cursor = self.input_cursor();
        if cursor < self.input.len() {
            self.input.remove(cursor);
            self.place_input_cursor(cursor);
        }
    }

    /// Insert pasted text as text. A paste never submits, so a clipboard with
    /// many lines becomes one multi-line draft instead of one launch per line.
    pub fn paste_input(&mut self, text: &str) {
        let text = normalize_pasted_text(text);
        if text.is_empty() {
            return;
        }
        if self.overlay == Overlay::None {
            self.start_new_session(None);
        }
        let single_line = self.input_is_single_line();
        for character in text.chars() {
            if single_line && matches!(character, '\n' | '\t') {
                self.push_input(' ');
            } else {
                self.push_input(character);
            }
        }
    }

    /// The model filter, session filter, local rename, and migration name are
    /// one-line fields: a line break or tab can never match or be accepted
    /// there, so a paste turns them into spaces.
    fn input_is_single_line(&self) -> bool {
        matches!(
            self.overlay,
            Overlay::ModelPicker
                | Overlay::Composer(
                    ComposerMode::Filter
                        | ComposerMode::Rename { .. }
                        | ComposerMode::MigrationName { .. }
                )
        )
    }

    /// Whether Enter would run a dashboard slash command rather than launch
    /// the draft. Only the command names `submit_new_session` handles count,
    /// so a pasted first line such as an absolute path is still a paste.
    pub fn draft_is_dashboard_command(&self) -> bool {
        if self.overlay != Overlay::Composer(ComposerMode::NewSession) {
            return false;
        }
        let command = self
            .input
            .trim()
            .split(char::is_whitespace)
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        DASHBOARD_COMMANDS.contains(&command.as_str())
    }

    pub fn pop_input(&mut self) {
        if self.overlay == Overlay::ModelPicker {
            self.model_filter.pop();
            self.reconcile_model_selection();
        } else if self.overlay == Overlay::HiddenPicker {
            self.hidden_filter.pop();
            self.reconcile_hidden_selection();
        } else if self.overlay == Overlay::DirectoryPicker {
            self.directory_filter.pop();
            self.directory_selection = 0;
        } else {
            let cursor = self.input_cursor();
            if let Some((index, _)) = self.input[..cursor].char_indices().next_back() {
                self.input.remove(index);
                if self.input_cursor.is_some() {
                    self.place_input_cursor(index);
                }
            }
        }
    }

    fn active_text_field(&mut self) -> &mut String {
        match self.overlay {
            Overlay::ModelPicker => &mut self.model_filter,
            Overlay::HiddenPicker => &mut self.hidden_filter,
            Overlay::DirectoryPicker => &mut self.directory_filter,
            _ => &mut self.input,
        }
    }

    fn reconcile_picker_selection(&mut self) {
        match self.overlay {
            Overlay::ModelPicker => self.reconcile_model_selection(),
            Overlay::HiddenPicker => self.reconcile_hidden_selection(),
            Overlay::DirectoryPicker => self.directory_selection = 0,
            _ => {}
        }
    }

    /// Delete the previous shell-style word in the active text field.
    ///
    /// Terminals report Option+Backspace as Alt+Backspace on macOS. Ctrl+W is
    /// kept as the portable equivalent. Work on Unicode scalar boundaries so
    /// slicing can never split a UTF-8 code point.
    pub fn delete_previous_word(&mut self) {
        let cursor = self.text_field_cursor();
        let input = self.active_text_field();
        let boundary = previous_word_boundary(input, cursor);
        input.replace_range(boundary..cursor, "");
        self.after_text_field_delete(boundary);
    }

    /// Delete from the cursor to the beginning of its line. Cmd+Backspace and
    /// Ctrl+U use this path.
    pub fn delete_to_line_start(&mut self) {
        let cursor = self.text_field_cursor();
        let input = self.active_text_field();
        let boundary = input[..cursor].rfind('\n').map_or(0, |index| index + 1);
        input.replace_range(boundary..cursor, "");
        self.after_text_field_delete(boundary);
    }

    /// Picker filters are always edited at their end.
    fn text_field_cursor(&self) -> usize {
        match self.overlay {
            Overlay::ModelPicker => self.model_filter.len(),
            Overlay::HiddenPicker => self.hidden_filter.len(),
            Overlay::DirectoryPicker => self.directory_filter.len(),
            _ => self.input_cursor(),
        }
    }

    fn after_text_field_delete(&mut self, cursor: usize) {
        if !matches!(
            self.overlay,
            Overlay::ModelPicker | Overlay::HiddenPicker | Overlay::DirectoryPicker
        ) && self.input_cursor.is_some()
        {
            self.place_input_cursor(cursor);
        }
        self.reconcile_picker_selection();
    }

    pub fn set_pins(&mut self, pins: BTreeMap<String, u64>) {
        self.pinned = pins;
        self.rebuild_group_cache();
    }

    pub fn set_sort_keys(&mut self, sort_keys: BTreeMap<String, u64>) {
        self.sort_keys = sort_keys;
        self.rebuild_group_cache();
    }

    fn directory_sort_key(&self, session: &AgentSession) -> u64 {
        self.sort_keys.get(&session.id).copied().unwrap_or_else(|| {
            session
                .started_at
                .and_then(|started| started.duration_since(SystemTime::UNIX_EPOCH).ok())
                .map_or(0, |since| since.as_millis() as u64)
        })
    }

    /// Swap the selected session with its neighbour in the same group.
    /// `delta` is -1 for up and 1 for down. Persistence is the caller's job.
    pub fn move_selected_session(&mut self, delta: isize) -> AppAction {
        let Some(session) = self.selected_session() else {
            self.set_notice("select a session to move");
            return AppAction::None;
        };
        let session_id = session.id.clone();
        let Some(index) = self.session_indices.get(&session_id).copied() else {
            return AppAction::None;
        };
        let Some(group) = self
            .groups()
            .iter()
            .find(|group| group.sessions.contains(&index))
        else {
            return AppAction::None;
        };
        let pinned = group.key == "pinned";
        if !pinned && self.view_mode != ViewMode::Directory {
            self.set_notice("reorder sessions in the directory view (ctrl+s) or pin them");
            return AppAction::None;
        }
        let position = group
            .sessions
            .iter()
            .position(|candidate| *candidate == index)
            .expect("group contains the selected session");
        let Some(target_position) = position
            .checked_add_signed(delta)
            .filter(|target| *target < group.sessions.len())
        else {
            self.set_notice(if delta < 0 {
                "already at the top of its group"
            } else {
                "already at the bottom of its group"
            });
            return AppAction::None;
        };
        let rows = group.sessions.clone();
        let ids: Vec<String> = rows
            .iter()
            .map(|row| self.snapshot.sessions[*row].id.clone())
            .collect();
        let before: Vec<u64> = rows
            .iter()
            .zip(&ids)
            .map(|(row, id)| {
                if pinned {
                    self.pinned.get(id).copied().unwrap_or(0)
                } else {
                    self.directory_sort_key(&self.snapshot.sessions[*row])
                }
            })
            .collect();
        // Groups sort by descending key with ties in row order. Make keys
        // strictly descending first, so swapping two always moves the rows.
        let mut after = before.clone();
        for index in (0..after.len().saturating_sub(1)).rev() {
            if after[index] <= after[index + 1] {
                after[index] = after[index + 1] + 1;
            }
        }
        after.swap(position, target_position);
        let keys: Vec<(String, u64)> = ids
            .into_iter()
            .zip(before.into_iter().zip(after))
            .filter(|(id, (old, new))| old != new || *id == session_id)
            .map(|(id, (_, new))| (id, new))
            .collect();
        let target = if pinned {
            &mut self.pinned
        } else {
            &mut self.sort_keys
        };
        for (id, key) in &keys {
            target.insert(id.clone(), *key);
        }
        self.rebuild_group_cache();
        self.select_and_reveal_session(&session_id);
        self.notice = None;
        AppAction::SetSortKeys { pinned, keys }
    }

    /// Pin or unpin the selected session and move it into or out of the
    /// leading Pinned group. Persistence is the caller's job.
    pub fn toggle_pin(&mut self) -> AppAction {
        let Some(session) = self.selected_session() else {
            self.set_notice("select a session to pin");
            return AppAction::None;
        };
        let session_id = session.id.clone();
        let name = session.name.clone();
        let pinned = if self.pinned.contains_key(&session_id) {
            self.pinned.remove(&session_id);
            false
        } else {
            let pinned_at = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;
            self.pinned.insert(session_id.clone(), pinned_at);
            true
        };
        self.rebuild_group_cache();
        // The row moved groups; keep it selected and on screen even when its
        // new group is collapsed or paged.
        self.select_and_reveal_session(&session_id);
        self.set_notice(if pinned {
            format!("pinned {name}")
        } else {
            format!("unpinned {name}")
        });
        AppAction::SetPin { session_id, pinned }
    }

    pub fn set_notice(&mut self, notice: impl Into<String>) {
        self.notice = Some(notice.into());
    }

    pub fn set_detail(&mut self, session_id: String, detail: String) {
        self.details.insert(session_id, detail);
    }

    pub fn selected_detail(&self) -> Option<&str> {
        let session = self.selected_session()?;
        self.details.get(&session.id).map(String::as_str)
    }

    pub fn filtered(&self, session: &AgentSession) -> bool {
        if self.filter.is_empty() {
            return true;
        }
        let needle = self.filter.to_ascii_lowercase();
        matches_filter(session, &needle)
    }

    fn status_groups(&self) -> Vec<Group> {
        let needle = self.filter.to_ascii_lowercase();
        let mut pinned = Vec::new();
        let mut grouped: BTreeMap<SessionState, Vec<usize>> = BTreeMap::new();
        for (index, session) in self.snapshot.sessions.iter().enumerate() {
            if !(needle.is_empty() || matches_filter(session, &needle)) {
                continue;
            }
            if self.pinned.contains_key(&session.id) {
                pinned.push(index);
            } else {
                grouped.entry(session.state).or_default().push(index);
            }
        }
        let mut groups = Vec::new();
        if let Some(group) = self.pinned_group(pinned) {
            groups.push(group);
        }
        groups.extend(SessionState::DISPLAY_ORDER.iter().filter_map(|state| {
            let sessions = grouped.remove(state).unwrap_or_default();
            (!sessions.is_empty()).then(|| Group {
                key: format!("state:{state:?}"),
                label: state.heading().into(),
                sessions,
            })
        }));
        groups
    }

    fn pinned_group(&self, mut sessions: Vec<usize>) -> Option<Group> {
        if sessions.is_empty() {
            return None;
        }
        sessions.sort_by(|left, right| {
            let left_at = self
                .pinned
                .get(&self.snapshot.sessions[*left].id)
                .copied()
                .unwrap_or(0);
            let right_at = self
                .pinned
                .get(&self.snapshot.sessions[*right].id)
                .copied()
                .unwrap_or(0);
            right_at.cmp(&left_at).then(left.cmp(right))
        });
        Some(Group {
            key: "pinned".into(),
            label: "Pinned".into(),
            sessions,
        })
    }

    fn directory_groups(&self) -> Vec<Group> {
        let needle = self.filter.to_ascii_lowercase();
        let mut pinned = Vec::new();
        let mut groups: BTreeMap<PathBuf, Vec<usize>> = BTreeMap::new();
        for (index, session) in self.snapshot.sessions.iter().enumerate() {
            if !(needle.is_empty() || matches_filter(session, &needle)) {
                continue;
            }
            if self.pinned.contains_key(&session.id) {
                pinned.push(index);
            } else {
                groups
                    .entry(project_group_path(&session.cwd))
                    .or_default()
                    .push(index);
            }
        }
        let mut ordered = Vec::new();
        if let Some(group) = self.pinned_group(pinned) {
            ordered.push(group);
        }
        ordered.extend(groups.into_iter().map(|(path, mut sessions)| {
            // Keep rows still while sessions change state, so replying to one
            // does not move it (and the cursor) elsewhere in its directory.
            sessions.sort_by(|left, right| {
                let left = &self.snapshot.sessions[*left];
                let right = &self.snapshot.sessions[*right];
                self.directory_sort_key(right)
                    .cmp(&self.directory_sort_key(left))
                    .then_with(|| left.id.cmp(&right.id))
            });
            Group {
                key: format!("cwd:{}", path.display()),
                label: abbreviate_home(&path),
                sessions,
            }
        }));
        ordered
    }

    fn rebuild_snapshot_cache(&mut self) {
        self.session_counts.clear();
        let mut provider_labels = BTreeSet::new();
        for session in &self.snapshot.sessions {
            *self.session_counts.entry(session.state).or_default() += 1;
            provider_labels.insert(session.provider.label().to_owned());
        }
        self.provider_labels = provider_labels.into_iter().collect();
        self.session_indices = self
            .snapshot
            .sessions
            .iter()
            .enumerate()
            .map(|(index, session)| (session.id.clone(), index))
            .collect();
        self.rebuild_group_cache();
    }

    fn rebuild_group_cache(&mut self) {
        self.group_cache = match self.view_mode {
            ViewMode::Status => self.status_groups(),
            ViewMode::Directory => self.directory_groups(),
        };
        #[cfg(test)]
        {
            self.group_cache_rebuilds += 1;
        }
    }

    fn reconcile_selection(&mut self) {
        let keys = self.selectable_keys();
        if self
            .selection
            .as_ref()
            .is_some_and(|selection| keys.contains(selection))
        {
            return;
        }
        self.selection = keys
            .iter()
            .find(|key| matches!(key, SelectionKey::Session(_)))
            .cloned()
            .or_else(|| keys.first().cloned());
    }

    /// Keep the cursor where the user was working when the selected row
    /// disappears (hidden, deleted, or gone after a refresh): prefer the row
    /// that followed it in the previous list order, then the one before it,
    /// and only then fall back to the top of the list.
    fn reconcile_selection_near(&mut self, previous_keys: &[SelectionKey]) {
        let keys = self.selectable_keys();
        let Some(selection) = self.selection.clone() else {
            return self.reconcile_selection();
        };
        if keys.contains(&selection) {
            return;
        }
        let Some(position) = previous_keys.iter().position(|key| *key == selection) else {
            return self.reconcile_selection();
        };
        let still_listed_session =
            |key: &&SelectionKey| matches!(key, SelectionKey::Session(_)) && keys.contains(key);
        let neighbour = previous_keys[position + 1..]
            .iter()
            .find(still_listed_session)
            .or_else(|| {
                previous_keys[..position]
                    .iter()
                    .rev()
                    .find(still_listed_session)
            })
            .cloned();
        match neighbour {
            Some(key) => self.selection = Some(key),
            None => self.reconcile_selection(),
        }
    }
}

impl App {
    /// Open the restore picker over the locally hidden registry, newest first.
    pub fn open_hidden_picker(&mut self, records: Vec<HiddenSessionRecord>) {
        self.show_restore_picker(records, false);
    }

    /// Open the restore picker over the hidden registry while the providers'
    /// saved sessions load; `add_restorable_sessions` merges them in.
    pub fn open_restore_picker(&mut self, records: Vec<HiddenSessionRecord>) {
        self.show_restore_picker(records, true);
    }

    fn show_restore_picker(&mut self, mut records: Vec<HiddenSessionRecord>, loading: bool) {
        if records.is_empty() && !loading {
            self.set_notice("no sessions are hidden locally");
            return;
        }
        records.sort_by(|left, right| right.hidden_at_ms.cmp(&left.hidden_at_ms));
        self.hidden_ids = records.iter().map(|record| record.id.clone()).collect();
        self.hidden_candidates = records;
        self.restorable.clear();
        self.restorable_loading = loading;
        self.hidden_filter.clear();
        self.hidden_selection = 0;
        self.notice = None;
        self.overlay = Overlay::HiddenPicker;
    }

    /// Merge saved provider sessions into the open picker, after the hidden
    /// ones and skipping rows the list already shows.
    pub fn add_restorable_sessions(&mut self, sessions: Vec<RestorableSession>, errors: &[String]) {
        if self.overlay != Overlay::HiddenPicker || !self.restorable_loading {
            return;
        }
        self.restorable_loading = false;
        let listed = self
            .snapshot
            .sessions
            .iter()
            .map(|session| session.id.as_str())
            .collect::<BTreeSet<_>>();
        let mut added = Vec::new();
        for session in sessions {
            if !self.hidden_ids.contains(&session.id) {
                if listed.contains(session.id.as_str()) {
                    continue;
                }
                added.push(HiddenSessionRecord {
                    id: session.id.clone(),
                    provider: Some(session.provider.clone()),
                    name: Some(session.name.clone()),
                    hidden_at_ms: session.updated_at_ms,
                });
            }
            self.restorable.insert(session.id.clone(), session);
        }
        self.hidden_candidates.extend(added);
        if !errors.is_empty() {
            self.set_notice(format!(
                "could not list every saved session: {}",
                errors.join("; ")
            ));
        }
        if self.hidden_candidates.is_empty() {
            self.overlay = Overlay::None;
            self.hidden_filter.clear();
            if errors.is_empty() {
                self.set_notice("no hidden or older sessions to restore");
            }
            return;
        }
        self.reconcile_hidden_selection();
    }

    /// Whether a picker row is in the local hidden registry, as opposed to a
    /// saved session that only fell out of the list.
    pub fn is_hidden_choice(&self, id: &str) -> bool {
        self.hidden_ids.contains(id)
    }

    pub fn hidden_choices(&self) -> Vec<&HiddenSessionRecord> {
        let needle = self.hidden_filter.to_ascii_lowercase();
        self.hidden_candidates
            .iter()
            .filter(|record| {
                needle.is_empty()
                    || record
                        .name
                        .as_deref()
                        .is_some_and(|name| name.to_ascii_lowercase().contains(&needle))
                    || record.provider.as_ref().is_some_and(|provider| {
                        provider.label().to_ascii_lowercase().contains(&needle)
                    })
                    || record.id.to_ascii_lowercase().contains(&needle)
                    || self.restorable.get(&record.id).is_some_and(|session| {
                        session
                            .cwd
                            .to_string_lossy()
                            .to_ascii_lowercase()
                            .contains(&needle)
                    })
            })
            .collect()
    }

    pub fn move_hidden_selection(&mut self, delta: isize) {
        let len = self.hidden_choices().len();
        if len == 0 {
            self.hidden_selection = 0;
            return;
        }
        self.hidden_selection =
            (self.hidden_selection as isize + delta).rem_euclid(len as isize) as usize;
    }

    /// Record how many result rows the picker renders for the current
    /// terminal height, so paging moves by what is actually on screen.
    pub fn set_hidden_picker_rows(&mut self, rows: usize) {
        self.hidden_picker_rows = rows.clamp(1, HIDDEN_PICKER_PAGE_SIZE);
    }

    pub fn move_hidden_page(&mut self, delta: isize) {
        let len = self.hidden_choices().len();
        if len == 0 {
            self.hidden_selection = 0;
            return;
        }
        self.hidden_selection = (self.hidden_selection as isize
            + delta * self.hidden_picker_rows as isize)
            .clamp(0, len.saturating_sub(1) as isize) as usize;
    }

    fn reconcile_hidden_selection(&mut self) {
        let len = self.hidden_choices().len();
        self.hidden_selection = self.hidden_selection.min(len.saturating_sub(1));
    }

    fn confirm_hidden_selection(&mut self) -> AppAction {
        let Some(record) = self.hidden_choices().get(self.hidden_selection).cloned() else {
            return AppAction::None;
        };
        let session_id = record.id.clone();
        let hidden = self.hidden_ids.contains(&session_id);
        let restorable = self.restorable.remove(&session_id);
        self.overlay = Overlay::None;
        self.hidden_filter.clear();
        self.hidden_candidates.clear();
        self.restorable.clear();
        self.restorable_loading = false;
        match restorable {
            Some(session) => AppAction::Restore { session, hidden },
            None => AppAction::Unhide { session_id },
        }
    }
}

/// Keep printable text, tabs, and line breaks from a paste. `\r\n` and bare
/// `\r` become `\n`; other control characters are dropped so a paste can never
/// carry escape sequences into the draft.
pub fn normalize_pasted_text(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut characters = text.chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            '\r' => {
                if characters.peek() == Some(&'\n') {
                    characters.next();
                }
                output.push('\n');
            }
            '\n' | '\t' => output.push(character),
            other if other.is_control() => {}
            other => output.push(other),
        }
    }
    output
}

fn matches_filter(session: &AgentSession, needle: &str) -> bool {
    session.name.to_ascii_lowercase().contains(needle)
        || session.summary.to_ascii_lowercase().contains(needle)
        || session
            .cwd
            .to_string_lossy()
            .to_ascii_lowercase()
            .contains(needle)
        || session
            .provider
            .label()
            .to_ascii_lowercase()
            .contains(needle)
}

impl App {
    fn submit_composer(&mut self, mode: ComposerMode) -> AppAction {
        let input = self.input.trim().to_owned();
        if input.is_empty() && !matches!(mode, ComposerMode::Filter | ComposerMode::Rename { .. }) {
            return AppAction::None;
        }
        if matches!(mode, ComposerMode::MigrationName { .. })
            && (input.len() > MAX_SESSION_NAME_BYTES || input.chars().any(char::is_control))
        {
            self.set_notice(format!(
                "migration name must contain 1 to {MAX_SESSION_NAME_BYTES} bytes without control characters"
            ));
            return AppAction::None;
        }
        self.clear_input();
        self.overlay = Overlay::None;
        match mode {
            ComposerMode::NewSession => self.submit_new_session(input),
            ComposerMode::Rename { session_id } => AppAction::Rename {
                session_id,
                name: input,
            },
            ComposerMode::MigrationName { session_id, target } => AppAction::Migrate {
                session_id,
                target,
                name: input,
            },
            ComposerMode::Filter => {
                self.set_filter(input);
                AppAction::None
            }
        }
    }

    fn confirm(&mut self, target: ConfirmTarget) -> AppAction {
        self.overlay = Overlay::None;
        match target {
            ConfirmTarget::Archive { id } => AppAction::Archive { session_id: id },
            ConfirmTarget::Hide { session_ids } => AppAction::Hide { session_ids },
            ConfirmTarget::Group { session_ids, .. } => AppAction::Delete { session_ids },
        }
    }

    fn request_open(&mut self) -> AppAction {
        let Some(session) = self.selected_session() else {
            return AppAction::None;
        };
        AppAction::Open {
            session_id: session.id.clone(),
        }
    }

    fn submit_new_session(&mut self, input: String) -> AppAction {
        if !input.starts_with('/') {
            if self.launch_provider == Provider::Antigravity && self.launch_model.is_none() {
                // Antigravity 1.1.x can terminate a task when its account does
                // not advertise a default PlanModel/RequestedModel. Never send
                // the user's task until an exact model has been selected.
                self.set_input(input);
                return self.open_model_picker();
            }
            let cwd = self.launch_directory();
            self.launch_directory_override = None;
            return AppAction::Launch {
                provider: self.launch_provider.clone(),
                model: self.launch_model.clone(),
                prompt: input,
                cwd,
            };
        }
        let (command, argument) = input
            .split_once(char::is_whitespace)
            .map(|(command, argument)| (command, argument.trim()))
            .unwrap_or((input.as_str(), ""));
        match command.to_ascii_lowercase().as_str() {
            "/help" => self.toggle_help(),
            "/harness" | "/provider" if argument.is_empty() => self.open_harness_picker(),
            "/harness" | "/provider" => self.select_launch_provider(argument),
            "/model" => return self.select_launch_model(argument),
            "/shell" if self.launch_provider == Provider::Terminal => {
                return self.select_launch_model(argument)
            }
            "/shell" => self.set_notice("/shell is available after selecting Terminal"),
            "/login" if argument.is_empty() => {
                return AppAction::Authenticate {
                    provider: self.launch_provider.clone(),
                }
            }
            "/login" => self.set_notice("use /login after selecting a harness with /harness"),
            "/setup" if argument.is_empty() => {
                return AppAction::SetupProvider {
                    provider: self.launch_provider.clone(),
                }
            }
            "/setup" => match known_provider(argument) {
                Some(provider) => return AppAction::SetupProvider { provider },
                None => self.set_notice(format!(
                    "unknown harness {argument}; use /help for supported setup names"
                )),
            },
            "/cd" => self.select_launch_directory(argument),
            "/completed" => return self.select_completed_visibility(argument),
            // Alternative to ctrl+g, which multiplexers such as Zellij bind
            // to their own lock mode before agentview can see it.
            "/hidden" | "/restore" => return AppAction::BrowseHidden,
            "/filter" => {
                self.set_filter(argument);
                self.set_notice(if argument.is_empty() {
                    "session filter cleared".into()
                } else {
                    format!("session filter: {argument}")
                });
            }
            _ => self.set_notice(format!("unknown dashboard command {command}; use /help")),
        }
        AppAction::None
    }

    fn select_launch_directory(&mut self, argument: &str) {
        if argument.is_empty() {
            self.launch_directory_override = None;
            self.set_notice("new sessions start in the selected or launch directory");
            return;
        }
        let path = match argument.strip_prefix('~') {
            Some(rest) if rest.is_empty() || rest.starts_with('/') => {
                match std::env::var_os("HOME") {
                    Some(home) => PathBuf::from(home).join(rest.trim_start_matches('/')),
                    None => {
                        self.set_notice("cannot expand ~ because HOME is not set");
                        return;
                    }
                }
            }
            _ => PathBuf::from(argument),
        };
        let path = if path.is_absolute() {
            path
        } else {
            match self
                .launch_directory()
                .or_else(|| std::env::current_dir().ok())
            {
                Some(base) => base.join(path),
                None => path,
            }
        };
        match path.canonicalize() {
            Ok(directory) if directory.is_dir() => {
                self.set_notice(format!("next session starts in {}", directory.display()));
                self.launch_directory_override = Some(directory);
            }
            _ => self.set_notice(format!("{} is not an existing directory", path.display())),
        }
    }

    /// Open the folder picker for the next new session. Candidates are the
    /// folders of known sessions (most recent first), then home and the
    /// projects in common code folders, so a folder without sessions is one
    /// search away.
    pub fn open_directory_picker(&mut self) {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        self.directory_candidates = directory_candidates(&self.snapshot.sessions, home.as_deref());
        self.directory_filter.clear();
        self.directory_selection = 0;
        self.directory_picker_from_composer =
            self.overlay == Overlay::Composer(ComposerMode::NewSession);
        self.notice = None;
        self.overlay = Overlay::DirectoryPicker;
    }

    /// Candidates matching the search. A typed absolute or `~` path that
    /// names an existing folder is offered first even when it is not a known
    /// candidate.
    pub fn directory_choices(&self) -> Vec<PathBuf> {
        let filter = self.directory_filter.trim();
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let typed = expand_typed_directory(filter, home.as_deref()).filter(|path| path.is_dir());
        let terms = filter
            .to_lowercase()
            .split_whitespace()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let matches = self
            .directory_candidates
            .iter()
            .filter(|path| typed.as_ref() != Some(*path))
            .filter(|path| {
                let label = display_directory(path, home.as_deref()).to_lowercase();
                terms.iter().all(|term| label.contains(term.as_str()))
            })
            .cloned();
        typed.clone().into_iter().chain(matches).collect()
    }

    pub fn move_directory_selection(&mut self, delta: isize) {
        let len = self.directory_choices().len();
        if len == 0 {
            self.directory_selection = 0;
            return;
        }
        self.directory_selection =
            (self.directory_selection as isize + delta).rem_euclid(len as isize) as usize;
    }

    pub fn move_directory_page(&mut self, delta: isize) {
        let len = self.directory_choices().len();
        if len == 0 {
            self.directory_selection = 0;
            return;
        }
        self.directory_selection = (self.directory_selection as isize
            + delta * self.hidden_picker_rows as isize)
            .clamp(0, len.saturating_sub(1) as isize) as usize;
    }

    fn confirm_directory_selection(&mut self) {
        let Some(directory) = self
            .directory_choices()
            .get(self.directory_selection)
            .cloned()
        else {
            return;
        };
        let directory = directory.canonicalize().unwrap_or(directory);
        let from_composer = self.directory_picker_from_composer;
        self.launch_directory_override = Some(directory);
        self.directory_filter.clear();
        self.directory_candidates.clear();
        if from_composer {
            self.overlay = Overlay::Composer(ComposerMode::NewSession);
            self.notice = None;
        } else {
            self.start_new_session(None);
        }
    }

    fn close_directory_picker(&mut self) {
        self.overlay = if self.directory_picker_from_composer {
            Overlay::Composer(ComposerMode::NewSession)
        } else {
            Overlay::None
        };
        self.directory_filter.clear();
        self.directory_candidates.clear();
        self.notice = None;
    }

    fn select_launch_provider(&mut self, argument: &str) {
        if argument.is_empty() {
            let choices = self
                .launch_targets
                .iter()
                .map(|target| target.provider.label())
                .collect::<Vec<_>>()
                .join(", ");
            self.set_notice(if choices.is_empty() {
                "no launch-capable harness is configured".into()
            } else {
                format!("available harnesses: {choices}")
            });
            return;
        }
        let normalized = normalize_provider_name(argument);
        let selected = self.launch_targets.iter().find(|target| {
            normalize_provider_name(target.provider.label()) == normalized
                || provider_alias(&target.provider) == normalized
        });
        if let Some(target) = selected {
            self.launch_provider = target.provider.clone();
            self.harness_selection = self
                .launch_targets
                .iter()
                .position(|candidate| candidate.provider == target.provider)
                .unwrap_or(0);
            self.launch_model = None;
            self.set_notice(format!(
                "new tasks will use the {} harness with its default model",
                target.provider.label()
            ));
        } else {
            self.set_notice(format!(
                "{argument} is not an available harness; use /harness"
            ));
        }
    }

    fn select_launch_model(&mut self, argument: &str) -> AppAction {
        if !self
            .launch_target()
            .is_some_and(|target| target.supports_model)
        {
            self.set_notice(format!(
                "{} does not expose model selection",
                self.launch_provider.label()
            ));
            return AppAction::None;
        }
        if argument.is_empty() {
            return self.open_model_picker();
        }
        if argument.eq_ignore_ascii_case("default") {
            self.launch_model = None;
            self.set_notice(format!(
                "{} will use its default {}",
                self.launch_provider.label(),
                if self.launch_provider == Provider::Terminal {
                    "shell"
                } else {
                    "model"
                }
            ));
            return AppAction::None;
        }
        if argument.len() > 128
            || argument
                .chars()
                .any(|character| character.is_control() || character.is_whitespace())
        {
            self.set_notice(if self.launch_provider == Provider::Terminal {
                "shell names must be 1–128 characters without whitespace"
            } else {
                "model names must be 1–128 characters without whitespace"
            });
            return AppAction::None;
        }
        self.launch_model = Some(argument.to_owned());
        self.set_notice(format!(
            "new {} tasks will use {} {argument}",
            self.launch_provider.label(),
            if self.launch_provider == Provider::Terminal {
                "shell"
            } else {
                "model"
            }
        ));
        AppAction::None
    }

    fn select_completed_visibility(&mut self, argument: &str) -> AppAction {
        let include_completed = match argument.to_ascii_lowercase().as_str() {
            "" | "toggle" => !self.includes_completed,
            "show" | "on" | "all" => true,
            "hide" | "off" => false,
            _ => {
                self.set_notice("use /completed, /completed show, or /completed hide");
                return AppAction::None;
            }
        };
        AppAction::SetCompletedVisibility { include_completed }
    }

    fn reconcile_model_selection(&mut self) {
        let len = self.model_choices().len();
        self.model_selection = self.model_selection.min(len.saturating_sub(1));
    }

    fn show_more(&mut self, key: &str) {
        let Some(group) = self.groups().iter().find(|group| group.key == key).cloned() else {
            self.reconcile_selection();
            return;
        };
        let previously_visible = self.visible_session_count(&group);
        let newly_visible = previously_visible
            .saturating_add(self.session_page_size)
            .min(group.sessions.len());
        let first_revealed = group
            .sessions
            .get(previously_visible)
            .map(|index| self.snapshot.sessions[*index].id.clone());

        self.visible_limits.insert(key.to_owned(), newly_visible);
        self.selection = first_revealed.map(SelectionKey::Session);
        self.notice = None;
        self.reconcile_selection();
    }
}

fn default_migration_name(name: &str, target: &str) -> String {
    let suffix = format!(" ({target})");
    let available = MAX_SESSION_NAME_BYTES.saturating_sub(suffix.len());
    let mut boundary = name.len().min(available);
    while !name.is_char_boundary(boundary) {
        boundary = boundary.saturating_sub(1);
    }
    format!("{}{}", name[..boundary].trim_end(), suffix)
}

pub(crate) fn is_active_session_state(state: SessionState) -> bool {
    state != SessionState::Completed
}

fn normalize_provider_name(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn provider_alias(provider: &Provider) -> String {
    match provider {
        Provider::GitHubCopilot => "copilot".into(),
        Provider::OpenCode => "opencode".into(),
        Provider::MistralVibe => "vibe".into(),
        Provider::MuseCode => "muse".into(),
        Provider::QwenCode => "qwen".into(),
        Provider::KimiCode => "kimi".into(),
        Provider::OhMyPi => "omp".into(),
        Provider::KiloCode => "kilo".into(),
        Provider::OpenHands => "openhands".into(),
        Provider::Hermes => "hermes".into(),
        Provider::MastraCode => "mastracode".into(),
        Provider::Devin => "devin".into(),
        _ => normalize_provider_name(provider.label()),
    }
}

fn known_provider(value: &str) -> Option<Provider> {
    match normalize_provider_name(value).as_str() {
        "claude" | "claudecode" => Some(Provider::Claude),
        "codex" => Some(Provider::Codex),
        "pi" => Some(Provider::Pi),
        "opencode" => Some(Provider::OpenCode),
        "cursor" | "cursoragent" => Some(Provider::Cursor),
        "copilot" | "githubcopilot" => Some(Provider::GitHubCopilot),
        "antigravity" | "agy" => Some(Provider::Antigravity),
        "mistral" | "mistralvibe" | "vibe" => Some(Provider::MistralVibe),
        "muse" | "musecode" => Some(Provider::MuseCode),
        "qwen" | "qwencode" => Some(Provider::QwenCode),
        "kimi" | "kimicode" => Some(Provider::KimiCode),
        "omp" | "ohmypi" => Some(Provider::OhMyPi),
        "grok" | "grokbuild" => Some(Provider::Grok),
        "kilo" | "kilocode" => Some(Provider::KiloCode),
        "openhands" => Some(Provider::OpenHands),
        "hermes" | "hermesagent" => Some(Provider::Hermes),
        "mastra" | "mastracode" => Some(Provider::MastraCode),
        "devin" => Some(Provider::Devin),
        "terminal" | "shell" => Some(Provider::Terminal),
        _ => None,
    }
}

fn valid_model_name(model: &str) -> bool {
    !model.is_empty()
        && model.len() <= 128
        && !model
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
}

/// Folders that typically hold one project per child directory.
const PROJECT_ROOTS: &[&str] = &[
    "Code",
    "code",
    "Projects",
    "projects",
    "src",
    "dev",
    "Developer",
];

fn directory_candidates(sessions: &[AgentSession], home: Option<&Path>) -> Vec<PathBuf> {
    let mut recent = sessions
        .iter()
        .map(|session| (session.updated_at, project_group_path(&session.cwd)))
        .collect::<Vec<_>>();
    recent.sort_by(|left, right| right.0.cmp(&left.0));
    let mut candidates = Vec::new();
    let mut push = |path: PathBuf| {
        if !candidates.contains(&path) && path.is_dir() {
            candidates.push(path);
        }
    };
    for (_, path) in recent {
        push(path);
    }
    if let Some(home) = home {
        push(home.to_path_buf());
        let mut read_roots = Vec::new();
        for root in PROJECT_ROOTS {
            let root = home.join(root);
            // Case-insensitive filesystems resolve Code and code to one folder.
            let Some(identity) = directory_identity(&root) else {
                continue;
            };
            if read_roots.contains(&identity) {
                continue;
            }
            read_roots.push(identity);
            let Ok(entries) = std::fs::read_dir(&root) else {
                continue;
            };
            let mut children = entries
                .filter_map(Result::ok)
                .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
                .filter(|entry| !entry.file_name().to_string_lossy().starts_with('.'))
                .map(|entry| entry.path())
                .collect::<Vec<_>>();
            children.sort();
            for child in children {
                push(child);
            }
        }
    }
    candidates
}

#[cfg(unix)]
fn directory_identity(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let metadata = std::fs::metadata(path)
        .ok()
        .filter(|metadata| metadata.is_dir())?;
    Some((metadata.dev(), metadata.ino()))
}

#[cfg(not(unix))]
fn directory_identity(path: &Path) -> Option<PathBuf> {
    path.canonicalize().ok().filter(|path| path.is_dir())
}

fn expand_typed_directory(input: &str, home: Option<&Path>) -> Option<PathBuf> {
    if Path::new(input).is_absolute() {
        return Some(PathBuf::from(input));
    }
    let rest = input.strip_prefix('~')?;
    if !rest.is_empty() && !rest.starts_with('/') {
        return None;
    }
    Some(home?.join(rest.trim_start_matches('/')))
}

/// `~`-relative label used for searching and showing picker folders.
pub fn display_directory(path: &Path, home: Option<&Path>) -> String {
    match home.and_then(|home| path.strip_prefix(home).ok()) {
        Some(rest) if rest.as_os_str().is_empty() => "~".into(),
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

pub(crate) fn project_group_path(path: &std::path::Path) -> PathBuf {
    let components = path.components().collect::<Vec<_>>();
    let worktree_marker = components
        .windows(2)
        .position(|pair| pair[0].as_os_str() == ".claude" && pair[1].as_os_str() == "worktrees");
    worktree_marker
        .map(|index| components[..index].iter().collect())
        .unwrap_or_else(|| path.to_path_buf())
}

fn abbreviate_home(path: &std::path::Path) -> String {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .and_then(|home| path.strip_prefix(home).ok().map(PathBuf::from))
        .map(|relative| format!("~/{}", relative.display()))
        .unwrap_or_else(|| path.display().to_string())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use crate::domain::{Capability, Provider, Runtime, SessionKind};

    use super::*;

    fn session(id: &str, state: SessionState) -> AgentSession {
        AgentSession {
            id: id.into(),
            provider_session_id: id.into(),
            provider: Provider::Claude,
            runtime: Runtime::Host,
            kind: SessionKind::Background,
            name: id.into(),
            cwd: PathBuf::from("/work"),
            state,
            summary: format!("summary {id}"),
            raw_state: None,
            pid: None,
            started_at: None,
            updated_at: None,
            pull_requests: None,
            capabilities: BTreeSet::from([Capability::Inspect]),
        }
    }

    #[test]
    fn navigation_wraps_across_headers_and_rows() {
        let mut app = App::new(SessionSnapshot {
            sessions: vec![
                session("one", SessionState::Working),
                session("two", SessionState::Completed),
            ],
            warnings: vec![],
        });

        assert_eq!(app.selection, Some(SelectionKey::Session("one".into())));
        app.selection = Some(SelectionKey::Session("two".into()));
        assert_eq!(app.selection, Some(SelectionKey::Session("two".into())));
        app.select_next();
        assert_eq!(
            app.selection,
            Some(SelectionKey::Group("state:Working".into()))
        );
    }

    #[test]
    fn pasting_many_lines_makes_one_draft_and_never_submits() {
        let mut app = App::new(SessionSnapshot {
            sessions: vec![session("one", SessionState::Working)],
            warnings: vec![],
        });

        app.paste_input("first line\r\nsecond line\rthird line\r");

        assert_eq!(app.overlay, Overlay::Composer(ComposerMode::NewSession));
        assert_eq!(app.input, "first line\nsecond line\nthird line\n");

        app.paste_input("\u{1b}[31mfour\u{7}th\t!");
        assert_eq!(
            app.input,
            "first line\nsecond line\nthird line\n[31mfourth\t!"
        );

        app.escape();
        app.paste_input("");
        assert_eq!(app.overlay, Overlay::None);
        assert!(app.input.is_empty());
    }

    #[test]
    fn pasting_into_the_model_picker_keeps_the_filter_on_one_line() {
        let mut app = App::new(SessionSnapshot::default());
        app.start_new_session(None);
        app.open_model_picker();

        app.paste_input("sonnet\r\nopus");

        assert_eq!(app.overlay, Overlay::ModelPicker);
        assert_eq!(app.model_filter, "sonnet opus");
    }

    #[test]
    fn pasting_into_single_line_fields_turns_line_breaks_and_tabs_into_spaces() {
        let single_line_overlays = [
            Overlay::ModelPicker,
            Overlay::Composer(ComposerMode::Filter),
            Overlay::Composer(ComposerMode::Rename {
                session_id: "one".into(),
            }),
            Overlay::Composer(ComposerMode::MigrationName {
                session_id: "one".into(),
                target: Provider::Codex,
            }),
        ];
        for overlay in single_line_overlays {
            let mut app = App::new(SessionSnapshot {
                sessions: vec![session("one", SessionState::Working)],
                warnings: vec![],
            });
            app.overlay = overlay.clone();

            app.paste_input("alpha\r\nbeta\tgamma\n");

            let field = if overlay == Overlay::ModelPicker {
                &app.model_filter
            } else {
                &app.input
            };
            assert_eq!(field, "alpha beta gamma ", "{overlay:?}");
        }

        let mut app = App::new(SessionSnapshot::default());
        app.start_new_session(None);
        app.paste_input("alpha\nbeta\tgamma");
        assert_eq!(app.input, "alpha\nbeta\tgamma");
    }

    #[test]
    fn only_known_dashboard_commands_count_as_commands() {
        for command in DASHBOARD_COMMANDS {
            let mut app = App::new(SessionSnapshot::default());
            app.start_new_session(None);
            app.input = format!("{} codex", command.to_ascii_uppercase());
            assert!(app.draft_is_dashboard_command(), "{command}");

            app.submit_new_session(command.to_string());
            assert!(
                !app.notice
                    .as_deref()
                    .is_some_and(|notice| notice.starts_with("unknown dashboard command")),
                "{command} is not handled by submit_new_session"
            );
        }

        let mut app = App::new(SessionSnapshot::default());
        app.start_new_session(None);
        for draft in ["/Users/me/notes.txt", "/tmp", "/", "fix /help", ""] {
            app.input = draft.into();
            assert!(!app.draft_is_dashboard_command(), "{draft}");
        }
        app.input = "/help".into();
        app.overlay = Overlay::Composer(ComposerMode::Filter);
        assert!(!app.draft_is_dashboard_command());
    }

    #[test]
    fn collapse_removes_group_children_from_navigation() {
        let mut app = App::new(SessionSnapshot {
            sessions: vec![session("one", SessionState::Working)],
            warnings: vec![],
        });
        app.selection = Some(SelectionKey::Group("state:Working".into()));

        app.activate();

        assert_eq!(app.selectable_keys().len(), 1);
    }

    #[test]
    fn large_groups_reveal_one_bounded_page_at_a_time() {
        let sessions = (0..61)
            .map(|index| session(&format!("session-{index:02}"), SessionState::Working))
            .collect();
        let mut app = app_with(sessions);
        let group = app.groups()[0].clone();

        assert_eq!(app.visible_session_count(&group), SESSION_PAGE_SIZE);
        assert_eq!(app.hidden_session_count(&group), 36);
        assert_eq!(app.selectable_keys().len(), 27);
        assert_eq!(
            app.selectable_keys().last(),
            Some(&SelectionKey::ShowMore("state:Working".into()))
        );

        app.selection = Some(SelectionKey::ShowMore("state:Working".into()));
        assert_eq!(app.activate(), AppAction::None);
        assert_eq!(
            app.selection,
            Some(SelectionKey::Session("session-25".into()))
        );
        assert_eq!(app.visible_session_count(&group), 50);
        assert_eq!(app.hidden_session_count(&group), 11);

        app.replace_snapshot(app.snapshot.clone());
        assert_eq!(app.visible_session_count(&group), 50);
        assert_eq!(
            app.selection,
            Some(SelectionKey::Session("session-25".into()))
        );

        app.selection = Some(SelectionKey::ShowMore("state:Working".into()));
        assert_eq!(app.activate(), AppAction::None);
        assert_eq!(
            app.selection,
            Some(SelectionKey::Session("session-50".into()))
        );
        assert_eq!(app.visible_session_count(&group), 61);
        assert_eq!(app.hidden_session_count(&group), 0);
        assert!(!app
            .selectable_keys()
            .iter()
            .any(|key| matches!(key, SelectionKey::ShowMore(_))));
    }

    #[test]
    fn post_launch_selection_expands_its_group_and_reveals_the_exact_row() {
        let sessions = (0..40)
            .map(|index| session(&format!("session-{index:02}"), SessionState::Working))
            .collect();
        let mut app = App::new(SessionSnapshot {
            sessions,
            warnings: Vec::new(),
        });
        app.set_session_page_size(5);
        let group_key = app.groups()[0].key.clone();
        app.collapsed.insert(group_key.clone());

        assert!(app.select_and_reveal_session("session-30"));

        assert_eq!(
            app.selection,
            Some(SelectionKey::Session("session-30".into()))
        );
        assert!(!app.collapsed.contains(&group_key));
        assert!(app
            .selectable_keys()
            .contains(&SelectionKey::Session("session-30".into())));
        assert!(!app.select_and_reveal_session("missing"));
    }

    #[test]
    fn visible_page_size_tracks_the_viewport_and_remains_bounded() {
        let sessions = (0..61)
            .map(|index| session(&format!("session-{index:02}"), SessionState::Working))
            .collect();
        let mut app = app_with(sessions);

        app.set_session_page_size(10);
        assert_eq!(app.session_page_size(), 10);
        assert_eq!(app.visible_session_count(&app.groups()[0]), 10);

        app.selection = Some(SelectionKey::ShowMore("state:Working".into()));
        assert_eq!(app.activate(), AppAction::None);
        assert_eq!(app.visible_session_count(&app.groups()[0]), 20);

        app.set_session_page_size(0);
        assert_eq!(app.session_page_size(), 1);
        assert_eq!(app.visible_session_count(&app.groups()[0]), 1);

        app.set_session_page_size(usize::MAX);
        assert_eq!(app.session_page_size(), SESSION_PAGE_SIZE);
        assert_eq!(
            app.visible_session_count(&app.groups()[0]),
            SESSION_PAGE_SIZE
        );
    }

    #[test]
    fn seventy_thousand_rows_are_grouped_once_across_navigation_and_draw_queries() {
        let sessions = (0..70_000)
            .map(|index| session(&format!("session-{index:05}"), SessionState::Completed))
            .collect();
        let mut app = app_with(sessions);
        assert!(app.includes_completed);
        let rebuilds = app.group_cache_rebuilds;

        for _ in 0..2_000 {
            app.select_next();
            let _ = app.groups();
            let _ = app.selected_session();
        }

        assert_eq!(app.group_cache_rebuilds, rebuilds);
        assert_eq!(app.groups()[0].sessions.len(), 70_000);
        assert!(app.selectable_keys().len() <= SESSION_PAGE_SIZE + 2);

        app.set_filter("session-69999");
        assert_eq!(app.group_cache_rebuilds, rebuilds + 1);
        assert_eq!(app.groups()[0].sessions, vec![69_999]);
    }

    #[test]
    fn live_animation_never_rebuilds_session_indexes() {
        let mut app = app_with(vec![
            session("live", SessionState::Working),
            session("done", SessionState::Completed),
        ]);
        let rebuilds = app.group_cache_rebuilds;
        assert!(app.live_animation_visible());

        assert!(app.advance_live_animation());
        assert!(!app.live_animation_visible());
        assert!(app.advance_live_animation());
        assert!(app.live_animation_visible());
        assert_eq!(app.group_cache_rebuilds, rebuilds);
    }

    fn remove_session(app: &mut App, id: &str) {
        let mut snapshot = app.snapshot.clone();
        snapshot.sessions.retain(|session| session.id != id);
        app.replace_snapshot(snapshot);
    }

    #[test]
    fn hiding_a_row_moves_the_cursor_to_its_neighbour_not_the_top() {
        let mut app = app_with(vec![
            session("needs-a", SessionState::NeedsInput),
            session("needs-b", SessionState::NeedsInput),
            session("done-a", SessionState::Completed),
            session("done-b", SessionState::Completed),
            session("done-c", SessionState::Completed),
        ]);

        // Middle of the completed group: the row that followed it takes over.
        app.selection = Some(SelectionKey::Session("done-b".into()));
        remove_session(&mut app, "done-b");
        assert_eq!(app.selection, Some(SelectionKey::Session("done-c".into())));

        // Last row of the list: fall back to the row before it, still in the
        // same group, rather than jumping to the first group.
        remove_session(&mut app, "done-c");
        assert_eq!(app.selection, Some(SelectionKey::Session("done-a".into())));

        // Last row of its group: the next group's first row is the neighbour.
        app.selection = Some(SelectionKey::Session("needs-b".into()));
        remove_session(&mut app, "needs-b");
        assert_eq!(app.selection, Some(SelectionKey::Session("done-a".into())));
    }

    #[test]
    fn hiding_the_last_paged_row_selects_the_row_that_slides_into_view() {
        let mut sessions = (0..26)
            .map(|index| session(&format!("work-{index:02}"), SessionState::Working))
            .collect::<Vec<_>>();
        sessions.push(session("done-a", SessionState::Completed));
        let mut app = app_with(sessions);
        assert_eq!(app.visible_session_count(&app.groups()[0].clone()), 25);

        // work-24 is the last row on the page; work-25 sits behind Show more.
        app.selection = Some(SelectionKey::Session("work-24".into()));
        remove_session(&mut app, "work-24");
        assert_eq!(
            app.selection,
            Some(SelectionKey::Session("work-25".into())),
            "the next row in the same group, not the next group"
        );
    }

    #[test]
    fn a_still_present_selection_survives_snapshot_replacement() {
        let mut app = app_with(vec![
            session("one", SessionState::Working),
            session("two", SessionState::Working),
        ]);
        app.selection = Some(SelectionKey::Session("two".into()));
        remove_session(&mut app, "one");
        assert_eq!(app.selection, Some(SelectionKey::Session("two".into())));
    }

    fn hidden(id: &str, name: &str, provider: Provider, hidden_at_ms: u64) -> HiddenSessionRecord {
        HiddenSessionRecord {
            id: id.into(),
            provider: Some(provider),
            name: Some(name.into()),
            hidden_at_ms,
        }
    }

    fn restorable(
        provider_session_id: &str,
        name: &str,
        cwd: &str,
        updated: u64,
    ) -> RestorableSession {
        RestorableSession {
            id: format!("opencode:host:{provider_session_id}"),
            provider_session_id: provider_session_id.into(),
            provider: Provider::OpenCode,
            name: name.into(),
            cwd: PathBuf::from(cwd),
            updated_at_ms: updated,
        }
    }

    #[test]
    fn restore_picker_adds_saved_sessions_the_list_lacks_and_restores_them() {
        let mut listed = session("opencode:host:ses_listed", SessionState::Working);
        listed.provider = Provider::OpenCode;
        let mut app = app_with(vec![listed]);
        app.open_restore_picker(Vec::new());
        assert_eq!(app.overlay, Overlay::HiddenPicker);
        assert!(app.restorable_loading);

        app.open_restore_picker(vec![hidden(
            "opencode:host:ses_hidden",
            "Hidden one",
            Provider::OpenCode,
            50,
        )]);
        app.add_restorable_sessions(
            vec![
                restorable("ses_listed", "Already listed", "/work/a", 90),
                restorable("ses_old", "Old arca work", "/work/arca", 40),
                restorable("ses_hidden", "Hidden one", "/work/b", 30),
            ],
            &[],
        );
        assert!(!app.restorable_loading);
        assert_eq!(
            app.hidden_choices()
                .iter()
                .map(|record| record.id.as_str())
                .collect::<Vec<_>>(),
            vec!["opencode:host:ses_hidden", "opencode:host:ses_old"]
        );
        assert!(app.is_hidden_choice("opencode:host:ses_hidden"));
        assert!(!app.is_hidden_choice("opencode:host:ses_old"));

        for character in "arca".chars() {
            app.push_input(character);
        }
        assert_eq!(app.hidden_choices().len(), 1);
        assert_eq!(
            app.activate(),
            AppAction::Restore {
                session: restorable("ses_old", "Old arca work", "/work/arca", 40),
                hidden: false,
            }
        );
        assert_eq!(app.overlay, Overlay::None);

        app.open_restore_picker(vec![hidden(
            "opencode:host:ses_hidden",
            "Hidden one",
            Provider::OpenCode,
            50,
        )]);
        app.add_restorable_sessions(
            vec![restorable("ses_hidden", "Hidden one", "/work/b", 30)],
            &[],
        );
        assert!(matches!(
            app.activate(),
            AppAction::Restore { hidden: true, .. }
        ));
    }

    #[test]
    fn restore_picker_with_nothing_to_offer_closes_with_a_notice() {
        let mut app = app_with(vec![session("live", SessionState::Working)]);
        app.open_restore_picker(Vec::new());
        app.add_restorable_sessions(Vec::new(), &[]);
        assert_eq!(app.overlay, Overlay::None);
        assert!(app
            .notice
            .as_deref()
            .unwrap()
            .contains("no hidden or older sessions"));
    }

    #[test]
    fn hidden_picker_searches_name_provider_and_id_then_restores_the_choice() {
        let mut app = app_with(vec![session("live", SessionState::Working)]);
        app.open_hidden_picker(vec![
            hidden(
                "claude:host:aaa",
                "Fix flaky auth test",
                Provider::Claude,
                10,
            ),
            hidden("cursor:host:bbb", "Agent Comparison", Provider::Cursor, 30),
            hidden("codex:host:ccc", "Billing retries", Provider::Codex, 20),
        ]);
        assert_eq!(app.overlay, Overlay::HiddenPicker);
        // Newest hide first.
        assert_eq!(
            app.hidden_choices()
                .iter()
                .map(|record| record.id.as_str())
                .collect::<Vec<_>>(),
            vec!["cursor:host:bbb", "codex:host:ccc", "claude:host:aaa"]
        );

        for character in "cursor".chars() {
            app.push_input(character);
        }
        assert_eq!(app.hidden_choices().len(), 1);
        app.delete_to_line_start();
        for character in "auth".chars() {
            app.push_input(character);
        }
        assert_eq!(app.hidden_choices()[0].id, "claude:host:aaa");
        app.delete_previous_word();
        for character in "ccc".chars() {
            app.push_input(character);
        }
        assert_eq!(app.hidden_choices()[0].id, "codex:host:ccc");

        assert_eq!(
            app.activate(),
            AppAction::Unhide {
                session_id: "codex:host:ccc".into()
            }
        );
        assert_eq!(app.overlay, Overlay::None);
        assert!(app.hidden_filter.is_empty());
        assert!(app.hidden_candidates.is_empty());
    }

    #[test]
    fn hidden_picker_selection_wraps_and_clamps_when_the_filter_shrinks() {
        let mut app = app_with(vec![]);
        app.open_hidden_picker(vec![
            hidden("a", "alpha", Provider::Claude, 3),
            hidden("b", "beta", Provider::Claude, 2),
            hidden("c", "gamma", Provider::Claude, 1),
        ]);
        app.move_hidden_selection(-1);
        assert_eq!(app.hidden_selection, 2);
        app.move_hidden_selection(1);
        assert_eq!(app.hidden_selection, 0);
        app.move_hidden_selection(2);
        for character in "a".chars() {
            app.push_input(character);
        }
        // "alpha", "beta", "gamma" all contain "a"; narrow further.
        app.push_input('m');
        assert_eq!(app.hidden_choices().len(), 1);
        assert_eq!(app.hidden_selection, 0);
        assert_eq!(
            app.escape(),
            AppAction::None,
            "esc closes the picker without quitting"
        );
        assert_eq!(app.overlay, Overlay::None);
    }

    #[test]
    fn picker_filters_ignore_newlines() {
        let mut app = app_with(vec![]);
        app.open_hidden_picker(vec![hidden("a", "alpha", Provider::Claude, 1)]);
        app.push_input('a');
        app.push_input('\n');
        app.push_input('\r');
        assert_eq!(app.hidden_filter, "a");
        assert_eq!(app.hidden_choices().len(), 1);
    }

    #[test]
    fn hidden_picker_pages_by_the_rendered_row_count() {
        let mut app = app_with(vec![]);
        app.open_hidden_picker(
            (0..12)
                .map(|index| hidden(&format!("id-{index}"), "row", Provider::Claude, index))
                .collect(),
        );
        // A 10-row terminal renders three result rows.
        app.set_hidden_picker_rows(hidden_picker_rows_for_height(10));
        app.move_hidden_page(1);
        assert_eq!(app.hidden_selection, 3);
        app.move_hidden_page(1);
        assert_eq!(app.hidden_selection, 6);
        app.move_hidden_page(-1);
        assert_eq!(app.hidden_selection, 3);

        app.set_hidden_picker_rows(hidden_picker_rows_for_height(60));
        app.move_hidden_page(1);
        assert_eq!(app.hidden_selection, 11, "clamped to the last choice");
    }

    #[test]
    fn slash_hidden_opens_the_restore_picker_like_ctrl_g() {
        let mut app = app_with(vec![]);
        app.start_new_session(None);
        app.input = "/hidden".into();
        assert_eq!(app.activate(), AppAction::BrowseHidden);
        assert_eq!(app.overlay, Overlay::None);
    }

    #[test]
    fn an_empty_hidden_registry_only_shows_a_notice() {
        let mut app = app_with(vec![]);
        app.open_hidden_picker(Vec::new());
        assert_eq!(app.overlay, Overlay::None);
        assert_eq!(
            app.notice.as_deref(),
            Some("no sessions are hidden locally")
        );
    }

    #[test]
    fn filtering_searches_hidden_sessions_and_resets_paging() {
        let sessions = (0..60)
            .map(|index| session(&format!("session-{index:02}"), SessionState::Working))
            .collect();
        let mut app = app_with(sessions);
        app.selection = Some(SelectionKey::ShowMore("state:Working".into()));
        app.activate();
        assert_eq!(app.visible_session_count(&app.groups()[0]), 50);

        app.start_filter();
        app.input = "session-59".into();
        assert_eq!(app.activate(), AppAction::None);

        let filtered = app.groups()[0].clone();
        assert_eq!(filtered.sessions, vec![59]);
        assert_eq!(app.visible_session_count(&filtered), 1);
        assert_eq!(
            app.selection,
            Some(SelectionKey::Session("session-59".into()))
        );

        app.start_filter();
        app.input.clear();
        app.activate();
        assert_eq!(
            app.visible_session_count(&app.groups()[0]),
            SESSION_PAGE_SIZE
        );
    }

    #[test]
    fn filter_matches_summary_case_insensitively() {
        let mut app = App::new(SessionSnapshot {
            sessions: vec![session("one", SessionState::Working)],
            warnings: vec![],
        });
        app.set_filter("SUMMARY ONE");

        assert_eq!(app.groups()[0].sessions, vec![0]);
    }

    #[test]
    fn escape_quits_directly_from_a_selected_row() {
        let mut app = App::new(SessionSnapshot {
            sessions: vec![session("one", SessionState::Working)],
            warnings: vec![],
        });

        assert_eq!(app.escape(), AppAction::Quit);
        assert!(app.should_quit);
    }

    #[test]
    fn filter_reconciles_a_selection_that_is_no_longer_visible() {
        let mut app = App::new(SessionSnapshot {
            sessions: vec![
                session("one", SessionState::Working),
                session("two", SessionState::Completed),
            ],
            warnings: vec![],
        });
        app.start_filter();
        app.input = "two".into();

        assert_eq!(app.activate(), AppAction::None);
        assert_eq!(app.selection, Some(SelectionKey::Session("two".into())));
    }

    #[test]
    fn claude_worktrees_group_under_the_owning_project() {
        let mut item = session("one", SessionState::Working);
        let (worktree, project) = if cfg!(windows) {
            (r"C:\repo\.claude\worktrees\topic\src", r"C:\repo")
        } else {
            ("/repo/.claude/worktrees/topic/src", "/repo")
        };
        item.cwd = PathBuf::from(worktree);
        let mut app = App::new(SessionSnapshot {
            sessions: vec![item],
            warnings: vec![],
        });
        app.toggle_view();

        assert_eq!(app.groups()[0].label, project);
        assert_eq!(app.groups()[0].key, format!("cwd:{project}"));
    }

    #[test]
    fn needs_input_is_treated_as_running_for_lifecycle_control() {
        let mut item = session("one", SessionState::NeedsInput);
        item.capabilities.insert(Capability::Interrupt);
        let mut app = App::new(SessionSnapshot {
            sessions: vec![item],
            warnings: vec![],
        });

        assert_eq!(
            app.start_confirm(),
            AppAction::Interrupt {
                session_id: "one".into()
            }
        );
        assert_eq!(app.overlay, Overlay::None);
    }

    #[test]
    fn active_groups_do_not_offer_bulk_deletion() {
        let mut app = App::new(SessionSnapshot {
            sessions: vec![session("one", SessionState::Working)],
            warnings: vec![],
        });
        app.selection = Some(SelectionKey::Group("state:Working".into()));

        app.start_confirm();

        assert_eq!(app.overlay, Overlay::None);
        assert_eq!(
            app.notice.as_deref(),
            Some("bulk stop is unavailable; select one running session")
        );
    }

    #[test]
    fn archive_requires_capability_and_an_idle_session() {
        let mut item = session("one", SessionState::Completed);
        item.capabilities.insert(Capability::Archive);
        let mut app = App::new(SessionSnapshot {
            sessions: vec![item],
            warnings: vec![],
        });

        app.start_archive_confirm();

        assert_eq!(
            app.overlay,
            Overlay::Confirm(ConfirmTarget::Archive { id: "one".into() })
        );
        assert_eq!(
            app.activate(),
            AppAction::Archive {
                session_id: "one".into()
            }
        );
    }

    #[test]
    fn read_only_peek_cannot_accept_reply_text() {
        let mut app = App::new(SessionSnapshot {
            sessions: vec![session("one", SessionState::Working)],
            warnings: vec![],
        });
        app.toggle_peek();

        app.push_input('x');

        assert_eq!(app.overlay, Overlay::Peek);
        assert!(app.input.is_empty());
    }

    #[test]
    fn pending_approval_keys_require_the_exact_granted_decision() {
        let mut item = session("one", SessionState::NeedsInput);
        item.capabilities.insert(Capability::Decline);
        let mut app = App::new(SessionSnapshot {
            sessions: vec![item],
            warnings: vec![],
        });
        app.toggle_peek();

        assert_eq!(app.resolve_approval(true), AppAction::None);
        assert_eq!(
            app.resolve_approval(false),
            AppAction::ResolveApproval {
                session_id: "one".into(),
                accept: false,
            }
        );
    }

    #[test]
    fn structured_input_uses_a_distinct_action_from_turn_reply() {
        let mut item = session("one", SessionState::NeedsInput);
        item.capabilities.insert(Capability::Respond);
        let mut app = App::new(SessionSnapshot {
            sessions: vec![item],
            warnings: vec![],
        });
        app.toggle_peek();
        for character in "staging".chars() {
            app.push_input(character);
        }

        assert_eq!(
            app.activate(),
            AppAction::RespondInput {
                session_id: "one".into(),
                answer: "staging".into(),
            }
        );
    }

    fn app_with(items: Vec<AgentSession>) -> App {
        App::new(SessionSnapshot {
            sessions: items,
            warnings: vec![],
        })
    }

    fn grant(item: &mut AgentSession, capabilities: &[Capability]) {
        item.capabilities.extend(capabilities.iter().cloned());
    }

    #[test]
    fn empty_snapshots_have_no_selection_and_navigation_is_a_noop() {
        let mut app = app_with(vec![]);

        app.select_next();
        app.select_previous();

        assert_eq!(app.selection, None);
        assert_eq!(app.activate(), AppAction::None);
    }

    #[test]
    fn reverse_navigation_from_no_selection_wraps_to_the_last_row() {
        let mut app = app_with(vec![
            session("one", SessionState::Working),
            session("two", SessionState::Completed),
        ]);
        app.selection = None;
        app.notice = Some("old notice".into());

        app.select_previous();

        assert_eq!(app.selection, Some(SelectionKey::Session("two".into())));
        assert_eq!(app.notice, None);
    }

    #[test]
    fn status_groups_follow_display_order_and_exclude_filtered_rows() {
        let mut app = app_with(vec![
            session("done", SessionState::Completed),
            session("working", SessionState::Working),
            session("input", SessionState::NeedsInput),
            session("review", SessionState::ReadyForReview),
        ]);

        assert_eq!(
            app.groups()
                .iter()
                .map(|group| group.label.as_str())
                .collect::<Vec<_>>(),
            vec!["Ready for review", "Needs input", "Working", "Completed"]
        );

        app.set_filter("summary working");
        assert_eq!(app.groups().len(), 1);
        assert_eq!(app.groups()[0].sessions, vec![1]);
    }

    #[test]
    fn pinning_lifts_a_session_into_a_leading_group_and_unpinning_restores_it() {
        let mut app = app_with(vec![
            session("needs", SessionState::NeedsInput),
            session("working", SessionState::Working),
            session("done", SessionState::Completed),
        ]);
        app.selection = Some(SelectionKey::Session("done".into()));
        assert_eq!(
            app.toggle_pin(),
            AppAction::SetPin {
                session_id: "done".into(),
                pinned: true,
            }
        );
        assert_eq!(app.groups()[0].label, "Pinned");
        assert_eq!(
            app.groups()[0]
                .sessions
                .iter()
                .map(|index| app.snapshot.sessions[*index].id.as_str())
                .collect::<Vec<_>>(),
            vec!["done"]
        );
        assert!(app.groups()[1..]
            .iter()
            .all(|group| group.label != "Completed"));

        app.selection = Some(SelectionKey::Session("working".into()));
        app.toggle_pin();
        assert_eq!(
            app.groups()[0]
                .sessions
                .iter()
                .map(|index| app.snapshot.sessions[*index].id.as_str())
                .collect::<Vec<_>>(),
            vec!["working", "done"]
        );

        app.selection = Some(SelectionKey::Session("done".into()));
        assert_eq!(
            app.toggle_pin(),
            AppAction::SetPin {
                session_id: "done".into(),
                pinned: false,
            }
        );
        assert_eq!(
            app.groups()
                .iter()
                .map(|group| group.label.as_str())
                .collect::<Vec<_>>(),
            vec!["Pinned", "Needs input", "Completed"]
        );
    }

    #[test]
    fn unpinning_reveals_the_row_in_a_collapsed_or_paged_group() {
        let mut app = app_with(vec![
            session("done-a", SessionState::Completed),
            session("done-b", SessionState::Completed),
            session("done-c", SessionState::Completed),
        ]);
        app.selection = Some(SelectionKey::Session("done-c".into()));
        app.toggle_pin();
        app.set_session_page_size(1);
        app.collapsed.insert("state:Completed".into());
        app.selection = Some(SelectionKey::Session("done-c".into()));

        app.toggle_pin();

        assert_eq!(app.selection, Some(SelectionKey::Session("done-c".into())));
        assert!(app
            .selectable_keys()
            .contains(&SelectionKey::Session("done-c".into())));
    }

    #[test]
    fn filter_matches_name_cwd_and_provider_and_rejects_unmatched_rows() {
        let mut item = session("agent-name", SessionState::Working);
        item.cwd = PathBuf::from("/projects/special-root");
        let mut app = app_with(vec![item]);

        for needle in ["AGENT-NAME", "SPECIAL-ROOT", "CLAUDE"] {
            app.set_filter(needle);
            assert_eq!(app.groups().len(), 1, "filter {needle}");
        }
        app.set_filter("missing");
        assert!(app.groups().is_empty());
    }

    #[test]
    fn directory_view_groups_and_sorts_project_paths() {
        let mut z = session("z", SessionState::Working);
        z.cwd = PathBuf::from("/zeta");
        let mut a = session("a", SessionState::Completed);
        a.cwd = PathBuf::from("/alpha");
        let mut a_worktree = session("a-worktree", SessionState::NeedsInput);
        a_worktree.cwd = PathBuf::from("/alpha/.claude/worktrees/topic/src");
        let mut app = app_with(vec![z, a, a_worktree]);

        app.toggle_view();

        assert_eq!(
            app.groups()
                .iter()
                .map(|group| (group.key.as_str(), group.sessions.clone()))
                .collect::<Vec<_>>(),
            vec![("cwd:/alpha", vec![1, 2]), ("cwd:/zeta", vec![0])]
        );
    }

    #[test]
    fn new_sessions_start_in_the_directory_selected_in_directory_view() {
        let mut home = session("home", SessionState::Completed);
        home.cwd = PathBuf::from("/home/user");
        let mut project = session("project", SessionState::Working);
        project.cwd = PathBuf::from("/home/user/code/arca");
        let mut app = app_with(vec![home, project]);

        app.start_new_session(None);
        app.input = "status view".into();
        assert!(matches!(
            app.activate(),
            AppAction::Launch { cwd: None, .. }
        ));

        app.toggle_view();
        app.selection = Some(SelectionKey::Group("cwd:/home/user".into()));
        app.start_new_session(None);
        app.input = "selected group".into();
        assert!(matches!(
            app.activate(),
            AppAction::Launch { cwd: Some(cwd), .. } if cwd == PathBuf::from("/home/user")
        ));

        app.selection = Some(SelectionKey::Session("project".into()));
        app.start_new_session(None);
        app.input = "selected session".into();
        assert!(matches!(
            app.activate(),
            AppAction::Launch { cwd: Some(cwd), .. } if cwd == PathBuf::from("/home/user/code/arca")
        ));
    }

    #[test]
    fn cd_starts_the_next_session_in_a_directory_without_sessions() {
        let root = std::env::temp_dir().join(format!("agentview-cd-{}", std::process::id()));
        std::fs::create_dir_all(root.join("fresh")).unwrap();
        let root = root.canonicalize().unwrap();
        let fresh = root.join("fresh");
        let mut app = app_with(vec![session("one", SessionState::Working)]);

        app.start_new_session(None);
        app.input = format!("/cd {}", root.join("missing").display());
        assert!(matches!(app.activate(), AppAction::None));
        assert_eq!(app.launch_directory(), None);

        app.start_new_session(None);
        app.input = format!("/cd {}", root.display());
        app.activate();
        app.start_new_session(None);
        app.input = "/cd fresh".into();
        app.activate();
        assert_eq!(app.launch_directory(), Some(fresh.clone()));

        app.start_new_session(None);
        app.input = "task".into();
        assert!(matches!(
            app.activate(),
            AppAction::Launch { cwd: Some(cwd), .. } if cwd == fresh
        ));
        assert_eq!(app.launch_directory(), None);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn directory_candidates_list_recent_session_folders_then_home_projects() {
        let home = crate::test_support::tempfile::tempdir().unwrap();
        let home = home.path().canonicalize().unwrap();
        for folder in ["Code/zeta", "Code/alpha", "Code/.hidden", "old", "recent"] {
            std::fs::create_dir_all(home.join(folder)).unwrap();
        }
        let at = |seconds| Some(SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(seconds));
        let mut old = session("old", SessionState::Completed);
        old.cwd = home.join("old");
        old.updated_at = at(10);
        let mut recent = session("recent", SessionState::Working);
        recent.cwd = home.join("recent");
        recent.updated_at = at(20);
        let mut gone = session("gone", SessionState::Working);
        gone.cwd = home.join("deleted");
        gone.updated_at = at(30);
        let mut project = session("project", SessionState::Working);
        project.cwd = home.join("Code/zeta");
        project.updated_at = at(5);

        let candidates = directory_candidates(&[old, recent, gone, project], Some(&home));

        assert_eq!(
            candidates,
            vec![
                home.join("recent"),
                home.join("old"),
                home.join("Code/zeta"),
                home.clone(),
                home.join("Code/alpha"),
            ]
        );
    }

    #[test]
    fn directory_picker_sets_the_folder_and_returns_to_the_draft() {
        let root = crate::test_support::tempfile::tempdir().unwrap();
        let root = root.path().canonicalize().unwrap();
        let picked = root.join("agentview-picker-target");
        std::fs::create_dir_all(&picked).unwrap();
        let mut item = session("one", SessionState::Working);
        item.cwd = picked.clone();
        let mut app = app_with(vec![item]);

        app.start_new_session(None);
        app.input = "fix the bug".into();
        app.open_directory_picker();
        assert_eq!(app.overlay, Overlay::DirectoryPicker);
        for character in "picker-target".chars() {
            app.push_input(character);
        }
        assert_eq!(app.directory_choices(), vec![picked.clone()]);
        assert_eq!(app.activate(), AppAction::None);

        assert_eq!(app.overlay, Overlay::Composer(ComposerMode::NewSession));
        assert_eq!(app.input, "fix the bug");
        assert_eq!(app.launch_directory(), Some(picked.clone()));
        assert!(matches!(
            app.activate(),
            AppAction::Launch { cwd: Some(cwd), .. } if cwd == picked
        ));
    }

    #[test]
    fn directory_picker_accepts_a_typed_path_and_escape_goes_back() {
        let root = crate::test_support::tempfile::tempdir().unwrap();
        let root = root.path().canonicalize().unwrap();
        let typed = root.join("unlisted");
        std::fs::create_dir_all(&typed).unwrap();
        let mut app = app_with(vec![session("one", SessionState::Working)]);

        app.open_directory_picker();
        app.escape();
        assert_eq!(app.overlay, Overlay::None);
        assert_eq!(app.launch_directory_override, None);

        app.start_new_session(None);
        app.open_directory_picker();
        app.escape();
        assert_eq!(app.overlay, Overlay::Composer(ComposerMode::NewSession));

        app.overlay = Overlay::None;
        app.open_directory_picker();
        for character in typed.display().to_string().chars() {
            app.push_input(character);
        }
        assert_eq!(app.directory_choices().first(), Some(&typed));
        app.activate();
        assert_eq!(app.overlay, Overlay::Composer(ComposerMode::NewSession));
        assert_eq!(app.launch_directory(), Some(typed));
    }

    #[test]
    fn typed_directories_expand_home_and_ignore_other_users() {
        let home = Path::new("/home/me");
        assert_eq!(
            expand_typed_directory("~", Some(home)),
            Some(PathBuf::from("/home/me"))
        );
        assert_eq!(
            expand_typed_directory("~/Code/app", Some(home)),
            Some(PathBuf::from("/home/me/Code/app"))
        );
        assert_eq!(expand_typed_directory("~other/app", Some(home)), None);
        assert_eq!(expand_typed_directory("app", Some(home)), None);
        assert_eq!(display_directory(Path::new("/home/me"), Some(home)), "~");
        assert_eq!(
            display_directory(Path::new("/home/me/Code/app"), Some(home)),
            "~/Code/app"
        );
    }

    #[test]
    fn activating_a_group_toggles_collapse_both_directions() {
        let mut app = app_with(vec![session("one", SessionState::Working)]);
        app.selection = Some(SelectionKey::Group("state:Working".into()));

        assert_eq!(app.activate(), AppAction::None);
        assert!(app.collapsed.contains("state:Working"));
        assert_eq!(app.activate(), AppAction::None);
        assert!(!app.collapsed.contains("state:Working"));
    }

    #[test]
    fn replacing_snapshot_preserves_valid_selection_but_closes_stale_overlay() {
        let mut app = app_with(vec![
            session("one", SessionState::Working),
            session("two", SessionState::Completed),
        ]);
        app.selection = Some(SelectionKey::Session("two".into()));
        app.overlay = Overlay::Peek;
        app.input = "draft".into();
        app.replace_snapshot(SessionSnapshot {
            sessions: vec![session("two", SessionState::Completed)],
            warnings: vec![],
        });
        assert_eq!(app.selection, Some(SelectionKey::Session("two".into())));
        assert_eq!(app.overlay, Overlay::Peek);
        assert_eq!(app.input, "draft");

        app.replace_snapshot(SessionSnapshot {
            sessions: vec![session("three", SessionState::Working)],
            warnings: vec![],
        });
        assert_eq!(app.selection, Some(SelectionKey::Session("three".into())));
        assert_eq!(app.overlay, Overlay::None);
        assert!(app.input.is_empty());
    }

    #[test]
    fn activate_covers_help_session_and_peek_without_selection() {
        let mut app = app_with(vec![session("one", SessionState::Working)]);
        app.overlay = Overlay::Help;
        assert_eq!(app.activate(), AppAction::None);
        assert_eq!(app.overlay, Overlay::None);

        app.overlay = Overlay::None;
        assert_eq!(
            app.activate(),
            AppAction::Open {
                session_id: "one".into()
            }
        );

        app.overlay = Overlay::Peek;
        app.selection = None;
        assert_eq!(app.activate(), AppAction::None);
    }

    #[test]
    fn enter_opens_every_provider_natively_even_when_inline_actions_exist() {
        for provider in [
            Provider::Claude,
            Provider::Codex,
            Provider::Pi,
            Provider::OpenCode,
            Provider::Cursor,
            Provider::GitHubCopilot,
            Provider::Antigravity,
            Provider::MistralVibe,
            Provider::MuseCode,
            Provider::QwenCode,
            Provider::KimiCode,
            Provider::OhMyPi,
            Provider::Grok,
            Provider::KiloCode,
            Provider::OpenHands,
        ] {
            for inline_capability in [
                Capability::Reply,
                Capability::Approve,
                Capability::Decline,
                Capability::Respond,
            ] {
                let mut item = session("managed", SessionState::Working);
                item.provider = provider.clone();
                item.capabilities = BTreeSet::from([Capability::Inspect, inline_capability]);
                let mut app = app_with(vec![item]);

                assert_eq!(
                    app.activate(),
                    AppAction::Open {
                        session_id: "managed".into()
                    }
                );
                assert_eq!(app.overlay, Overlay::None);
            }
        }

        let mut read_only = session("external", SessionState::Completed);
        read_only.provider = Provider::Pi;
        read_only.capabilities = BTreeSet::from([Capability::Inspect]);
        let mut app = app_with(vec![read_only]);
        assert_eq!(
            app.activate(),
            AppAction::Open {
                session_id: "external".into()
            }
        );
        assert_eq!(app.overlay, Overlay::None);
    }

    #[test]
    fn migration_picker_excludes_source_and_prefills_an_editable_local_name() {
        let mut item = session("native-id", SessionState::Completed);
        item.name = "review API".into();
        item.provider = Provider::Claude;
        let mut app = app_with(vec![item]);

        app.start_migration();
        assert_eq!(
            app.overlay,
            Overlay::MigrationTargetPicker {
                session_id: "native-id".into()
            }
        );
        assert_eq!(
            app.migration_targets.len(),
            Provider::CODING_HARNESS_COUNT - 1
        );
        assert!(!app.migration_targets.contains(&Provider::Claude));
        assert_eq!(app.migration_targets[0], Provider::Codex);

        assert_eq!(app.activate(), AppAction::None);
        assert_eq!(app.input, "review API (Codex)");
        assert_eq!(
            app.overlay,
            Overlay::Composer(ComposerMode::MigrationName {
                session_id: "native-id".into(),
                target: Provider::Codex,
            })
        );
        app.input = "review API port".into();
        assert_eq!(
            app.activate(),
            AppAction::Migrate {
                session_id: "native-id".into(),
                target: Provider::Codex,
                name: "review API port".into(),
            }
        );
        assert_eq!(app.overlay, Overlay::None);
    }

    #[test]
    fn migration_escape_returns_to_targets_before_closing_and_rejects_terminal() {
        let mut app = app_with(vec![session("source", SessionState::Completed)]);
        app.start_migration();
        app.move_migration_selection(-1);
        assert_eq!(
            app.migration_targets[app.migration_selection],
            Provider::Devin
        );
        app.confirm_migration_target();
        assert!(matches!(
            app.overlay,
            Overlay::Composer(ComposerMode::MigrationName { .. })
        ));
        app.escape();
        assert!(matches!(app.overlay, Overlay::MigrationTargetPicker { .. }));
        assert_eq!(app.migration_selection, Provider::CODING_HARNESS_COUNT - 2);
        app.escape();
        assert_eq!(app.overlay, Overlay::None);

        let mut terminal = session("terminal", SessionState::Completed);
        terminal.provider = Provider::Terminal;
        let mut app = app_with(vec![terminal]);
        app.start_migration();
        assert_eq!(app.overlay, Overlay::None);
        assert_eq!(
            app.notice.as_deref(),
            Some("Terminal sessions cannot be migrated")
        );

        let mut docker = session("docker", SessionState::Completed);
        docker.runtime = Runtime::Docker {
            container_id: "exact".into(),
            container_name: "agent".into(),
            image: "image@sha256:exact".into(),
        };
        let mut app = app_with(vec![docker]);
        app.start_migration();
        assert_eq!(app.overlay, Overlay::None);
        assert_eq!(
            app.notice.as_deref(),
            Some("session migration currently supports host sessions only")
        );
    }

    #[test]
    fn migration_default_name_is_utf8_safe_and_within_alias_limit() {
        let name = default_migration_name(&"é".repeat(200), "OpenHands");
        assert!(name.is_char_boundary(name.len()));
        assert!(name.len() <= MAX_SESSION_NAME_BYTES);
        assert!(name.ends_with(" (OpenHands)"));
    }

    #[test]
    fn escape_closes_every_overlay_before_it_quits() {
        let overlays = vec![
            Overlay::Help,
            Overlay::Peek,
            Overlay::Composer(ComposerMode::NewSession),
            Overlay::Confirm(ConfirmTarget::Archive { id: "one".into() }),
        ];
        for overlay in overlays {
            let mut app = app_with(vec![session("one", SessionState::Completed)]);
            app.overlay = overlay;
            app.input = "draft".into();
            assert_eq!(app.escape(), AppAction::None);
            assert_eq!(app.overlay, Overlay::None);
            assert!(app.input.is_empty());
            assert!(!app.should_quit);
            assert_eq!(app.escape(), AppAction::Quit);
            assert!(app.should_quit);
        }
    }

    #[test]
    fn toggling_view_clears_collapsed_groups_and_reconciles_selection() {
        let mut app = app_with(vec![session("one", SessionState::Working)]);
        app.selection = Some(SelectionKey::Group("state:Working".into()));
        app.collapsed.insert("state:Working".into());

        app.toggle_view();

        assert_eq!(app.view_mode, ViewMode::Directory);
        assert!(app.collapsed.is_empty());
        assert_eq!(app.selection, Some(SelectionKey::Session("one".into())));
        app.toggle_view();
        assert_eq!(app.view_mode, ViewMode::Status);
    }

    #[test]
    fn directory_view_orders_sessions_by_creation_not_state() {
        let started = |id: &str, state, secs| {
            let mut item = session(id, state);
            item.started_at = Some(SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(secs));
            item
        };
        let mut app = app_with(vec![
            started("old", SessionState::ReadyForReview, 1),
            started("new", SessionState::Working, 3),
            started("mid", SessionState::Completed, 2),
        ]);
        app.set_view_mode(ViewMode::Directory);
        let order = |app: &App| -> Vec<String> {
            app.group_cache[0]
                .sessions
                .iter()
                .map(|index| app.snapshot.sessions[*index].id.clone())
                .collect()
        };
        assert_eq!(order(&app), ["new", "mid", "old"]);

        let mut snapshot = app.snapshot.clone();
        snapshot.sessions[0].state = SessionState::Working;
        snapshot.sessions[1].state = SessionState::ReadyForReview;
        app.replace_snapshot(snapshot);
        assert_eq!(order(&app), ["new", "mid", "old"]);
    }

    #[test]
    fn moving_a_session_swaps_it_with_its_neighbour_and_keeps_new_ones_on_top() {
        let started = |id: &str, secs| {
            let mut item = session(id, SessionState::Working);
            item.started_at = Some(SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(secs));
            item
        };
        let mut app = app_with(vec![
            started("old", 1),
            started("new", 3),
            started("mid", 2),
        ]);
        app.set_view_mode(ViewMode::Directory);
        let order = |app: &App| -> Vec<String> {
            app.group_cache[0]
                .sessions
                .iter()
                .map(|index| app.snapshot.sessions[*index].id.clone())
                .collect()
        };
        app.selection = Some(SelectionKey::Session("old".into()));

        let AppAction::SetSortKeys { pinned, keys } = app.move_selected_session(-1) else {
            panic!("expected a sort-key action");
        };
        assert!(!pinned);
        assert_eq!(keys, [("mid".into(), 1000), ("old".into(), 2000)]);
        assert_eq!(order(&app), ["new", "old", "mid"]);
        app.move_selected_session(-1);
        assert_eq!(order(&app), ["old", "new", "mid"]);
        assert_eq!(app.move_selected_session(-1), AppAction::None);
        assert_eq!(app.selection, Some(SelectionKey::Session("old".into())));

        let mut snapshot = app.snapshot.clone();
        snapshot.sessions.push(started("newest", 4));
        app.replace_snapshot(snapshot);
        assert_eq!(order(&app), ["newest", "old", "new", "mid"]);
    }

    #[test]
    fn moving_sessions_without_start_times_moves_exactly_one_place() {
        let mut app = app_with(vec![
            session("a", SessionState::Working),
            session("b", SessionState::Working),
            session("c", SessionState::Working),
        ]);
        app.set_view_mode(ViewMode::Directory);
        let order = |app: &App| -> Vec<String> {
            app.group_cache[0]
                .sessions
                .iter()
                .map(|index| app.snapshot.sessions[*index].id.clone())
                .collect()
        };
        assert_eq!(order(&app), ["a", "b", "c"]);
        app.selection = Some(SelectionKey::Session("b".into()));
        app.move_selected_session(1);
        assert_eq!(order(&app), ["a", "c", "b"]);
        app.move_selected_session(-1);
        app.move_selected_session(-1);
        assert_eq!(order(&app), ["b", "a", "c"]);
    }

    #[test]
    fn moving_a_pinned_session_reorders_the_pinned_group() {
        let mut app = app_with(vec![
            session("one", SessionState::Working),
            session("two", SessionState::Working),
        ]);
        app.set_pins(BTreeMap::from([("one".into(), 20), ("two".into(), 10)]));
        app.selection = Some(SelectionKey::Session("two".into()));

        let AppAction::SetSortKeys { pinned, keys } = app.move_selected_session(-1) else {
            panic!("expected a sort-key action");
        };
        assert!(pinned);
        assert_eq!(keys, [("one".into(), 10), ("two".into(), 20)]);
        assert_eq!(app.groups()[0].label, "Pinned");
        assert_eq!(
            app.groups()[0]
                .sessions
                .iter()
                .map(|index| app.snapshot.sessions[*index].id.as_str())
                .collect::<Vec<_>>(),
            ["two", "one"]
        );
    }

    #[test]
    fn moving_an_unpinned_session_in_status_view_explains_where_it_works() {
        let mut app = app_with(vec![
            session("one", SessionState::Working),
            session("two", SessionState::Working),
        ]);
        app.set_view_mode(ViewMode::Status);
        app.selection = Some(SelectionKey::Session("two".into()));
        assert_eq!(app.move_selected_session(-1), AppAction::None);
        assert!(app
            .notice
            .as_deref()
            .is_some_and(|notice| notice.contains("directory view")));
    }

    #[test]
    fn peek_requires_inspection_and_toggle_close_discards_input() {
        let mut item = session("one", SessionState::Working);
        item.capabilities.clear();
        let mut app = app_with(vec![item]);

        app.toggle_peek();
        assert_eq!(app.overlay, Overlay::None);
        assert!(app
            .notice
            .as_deref()
            .unwrap()
            .contains("inspection was not granted"));

        grant(
            &mut app.snapshot.sessions[0],
            &[Capability::Inspect, Capability::Reply],
        );
        app.toggle_peek();
        app.input = "draft".into();
        app.toggle_peek();
        assert_eq!(app.overlay, Overlay::None);
        assert!(app.input.is_empty());
    }

    #[test]
    fn composer_entry_points_seed_the_expected_input() {
        let mut app = app_with(vec![session("one", SessionState::Completed)]);
        app.start_new_session(Some('x'));
        assert_eq!(app.input, "x");
        assert_eq!(app.overlay, Overlay::Composer(ComposerMode::NewSession));

        app.filter = "saved".into();
        app.start_filter();
        assert_eq!(app.input, "saved");
        assert_eq!(app.overlay, Overlay::Composer(ComposerMode::Filter));

        app.input = "leftover".into();
        app.start_rename();
        assert_eq!(app.input, "");
        assert_eq!(
            app.overlay,
            Overlay::Composer(ComposerMode::Rename {
                session_id: "one".into()
            })
        );

        app.selection = None;
        app.overlay = Overlay::None;
        app.start_rename();
        assert_eq!(app.overlay, Overlay::None);
    }

    #[test]
    fn every_composer_submission_maps_to_its_exact_action() {
        let mut app = app_with(vec![session("one", SessionState::Completed)]);

        app.start_new_session(None);
        app.input = "  build it  ".into();
        assert_eq!(
            app.activate(),
            AppAction::Launch {
                provider: Provider::Claude,
                model: None,
                prompt: "build it".into(),
                cwd: None,
            }
        );

        app.overlay = Overlay::Composer(ComposerMode::Rename {
            session_id: "one".into(),
        });
        app.input = "  new name  ".into();
        assert_eq!(
            app.activate(),
            AppAction::Rename {
                session_id: "one".into(),
                name: "new name".into()
            }
        );

        app.overlay = Overlay::Composer(ComposerMode::Rename {
            session_id: "one".into(),
        });
        app.input = "   ".into();
        assert_eq!(
            app.activate(),
            AppAction::Rename {
                session_id: "one".into(),
                name: String::new()
            }
        );

        app.filter = "old".into();
        app.start_filter();
        app.input = "   ".into();
        assert_eq!(app.activate(), AppAction::None);
        assert!(app.filter.is_empty());
        assert_eq!(app.overlay, Overlay::None);
    }

    #[test]
    fn empty_non_filter_composer_does_not_submit_or_close() {
        let mut app = app_with(vec![]);
        app.start_new_session(None);
        app.input = "   ".into();

        assert_eq!(app.activate(), AppAction::None);
        assert_eq!(app.overlay, Overlay::Composer(ComposerMode::NewSession));
        assert_eq!(app.input, "   ");
    }

    #[test]
    fn peek_reply_requires_capability_and_clears_rejected_or_sent_text() {
        let mut app = app_with(vec![session("one", SessionState::Working)]);
        app.toggle_peek();
        app.input = "cannot send".into();
        assert_eq!(app.activate(), AppAction::None);
        assert!(app.input.is_empty());
        assert!(app.notice.as_deref().unwrap().contains("read-only"));

        grant(&mut app.snapshot.sessions[0], &[Capability::Reply]);
        app.input = "  proceed  ".into();
        assert_eq!(
            app.activate(),
            AppAction::Reply {
                session_id: "one".into(),
                prompt: "proceed".into()
            }
        );
        assert!(app.input.is_empty());
    }

    #[test]
    fn active_and_idle_session_confirmations_mutate_only_with_authority_or_hide_locally() {
        let mut active = session("active", SessionState::ReadyForReview);
        active.capabilities.clear();
        let mut app = app_with(vec![active]);
        assert_eq!(app.start_confirm(), AppAction::None);
        assert_eq!(
            app.activate(),
            AppAction::Hide {
                session_ids: vec!["active".into()]
            }
        );

        grant(&mut app.snapshot.sessions[0], &[Capability::Interrupt]);
        assert_eq!(
            app.start_confirm(),
            AppAction::Interrupt {
                session_id: "active".into()
            }
        );

        app.snapshot.sessions[0].state = SessionState::Completed;
        app.snapshot.sessions[0].capabilities.clear();
        assert_eq!(
            app.start_confirm(),
            AppAction::Hide {
                session_ids: vec!["active".into()]
            }
        );
        grant(&mut app.snapshot.sessions[0], &[Capability::Delete]);
        assert_eq!(
            app.start_confirm(),
            AppAction::Delete {
                session_ids: vec!["active".into()]
            }
        );
    }

    #[test]
    fn completed_group_never_mixes_provider_deletion_with_local_hiding() {
        let mut one = session("one", SessionState::Completed);
        let two = session("two", SessionState::Completed);
        grant(&mut one, &[Capability::Delete]);
        let mut app = app_with(vec![one, two]);
        app.selection = Some(SelectionKey::Group("state:Completed".into()));

        app.start_confirm();
        assert_eq!(
            app.activate(),
            AppAction::Hide {
                session_ids: vec!["two".into()]
            }
        );

        grant(&mut app.snapshot.sessions[1], &[Capability::Delete]);
        app.start_confirm();
        assert_eq!(
            app.activate(),
            AppAction::Delete {
                session_ids: vec!["one".into(), "two".into()]
            }
        );
    }

    #[test]
    fn archive_refuses_active_or_unowned_sessions() {
        let mut item = session("one", SessionState::Working);
        grant(&mut item, &[Capability::Archive]);
        let mut app = app_with(vec![item]);
        app.start_archive_confirm();
        assert!(app.notice.as_deref().unwrap().contains("must be idle"));
        assert_eq!(app.overlay, Overlay::None);

        app.snapshot.sessions[0].state = SessionState::Completed;
        app.snapshot.sessions[0].capabilities.clear();
        app.start_archive_confirm();
        assert!(app.notice.as_deref().unwrap().contains("Archive authority"));
    }

    #[test]
    fn approval_resolution_requires_peek_selection_and_exact_capability() {
        let mut item = session("one", SessionState::NeedsInput);
        grant(&mut item, &[Capability::Approve, Capability::Decline]);
        let mut app = app_with(vec![item]);

        assert_eq!(app.resolve_approval(true), AppAction::None);
        app.toggle_peek();
        assert_eq!(
            app.resolve_approval(true),
            AppAction::ResolveApproval {
                session_id: "one".into(),
                accept: true
            }
        );
        app.selection = None;
        assert_eq!(app.resolve_approval(false), AppAction::None);
    }

    #[test]
    fn input_editing_is_limited_to_composers_and_writable_peek() {
        let mut app = app_with(vec![session("one", SessionState::Working)]);
        app.push_input('x');
        assert!(app.input.is_empty());

        app.start_new_session(None);
        app.push_input('a');
        app.push_input('b');
        app.pop_input();
        assert_eq!(app.input, "a");

        app.overlay = Overlay::Peek;
        app.input.clear();
        grant(&mut app.snapshot.sessions[0], &[Capability::Respond]);
        app.push_input('z');
        assert_eq!(app.input, "z");
    }

    #[test]
    fn details_are_selected_by_exact_session_id() {
        let mut app = app_with(vec![
            session("one", SessionState::Working),
            session("two", SessionState::Completed),
        ]);
        app.set_detail("one".into(), "first transcript".into());
        app.set_detail("two".into(), "second transcript".into());
        assert_eq!(app.selected_detail(), Some("first transcript"));
        app.selection = Some(SelectionKey::Session("two".into()));
        assert_eq!(app.selected_detail(), Some("second transcript"));
        app.selection = Some(SelectionKey::Group("state:Completed".into()));
        assert_eq!(app.selected_detail(), None);
    }

    #[test]
    fn successful_overlay_and_view_transitions_clear_stale_notices() {
        let mut item = session("one", SessionState::Completed);
        grant(
            &mut item,
            &[Capability::Inspect, Capability::Delete, Capability::Archive],
        );
        let mut app = app_with(vec![item]);

        app.notice = Some("stale".into());
        app.toggle_help();
        assert_eq!(app.notice, None);
        app.toggle_help();

        app.notice = Some("stale".into());
        app.toggle_view();
        assert_eq!(app.notice, None);

        app.notice = Some("stale".into());
        app.toggle_peek();
        assert_eq!(app.notice, None);
        app.escape();

        app.notice = Some("stale".into());
        app.start_new_session(None);
        assert_eq!(app.notice, None);
        app.escape();

        app.notice = Some("stale".into());
        app.start_filter();
        assert_eq!(app.notice, None);
        app.escape();

        app.notice = Some("stale".into());
        app.start_rename();
        assert_eq!(app.notice, None);
        app.escape();

        app.notice = Some("stale".into());
        app.start_confirm();
        assert_eq!(app.notice, None);
        app.escape();

        app.notice = Some("stale".into());
        app.start_archive_confirm();
        assert_eq!(app.notice, None);
    }

    #[test]
    fn refused_transition_replaces_stale_notice_with_the_refusal() {
        let mut item = session("one", SessionState::Completed);
        item.capabilities.clear();
        let mut app = app_with(vec![item]);
        app.notice = Some("stale".into());

        app.toggle_peek();

        assert_eq!(app.overlay, Overlay::None);
        assert_ne!(app.notice.as_deref(), Some("stale"));
        assert!(app
            .notice
            .as_deref()
            .unwrap()
            .contains("inspection was not granted"));
    }

    #[test]
    fn task_commands_select_provider_model_and_filter_without_launching() {
        let mut app = App::with_launch_targets(
            SessionSnapshot::default(),
            false,
            Provider::Claude,
            vec![
                LaunchTarget {
                    provider: Provider::Claude,
                    supports_model: true,
                },
                LaunchTarget {
                    provider: Provider::Pi,
                    supports_model: false,
                },
            ],
        );

        app.start_new_session(None);
        app.input = "/harness".into();
        assert_eq!(app.activate(), AppAction::None);
        assert_eq!(app.overlay, Overlay::HarnessPicker);
        assert_eq!(app.escape(), AppAction::None);
        assert_eq!(app.overlay, Overlay::Composer(ComposerMode::NewSession));

        app.start_new_session(None);
        app.input = "/harness pi".into();
        assert_eq!(app.activate(), AppAction::None);
        assert_eq!(app.launch_provider, Provider::Pi);

        app.start_new_session(None);
        app.input = "/model opus".into();
        assert_eq!(app.activate(), AppAction::None);
        assert!(app.launch_model.is_none());
        assert!(app.notice.as_deref().unwrap().contains("does not expose"));

        app.start_new_session(None);
        app.input = "/provider claude".into();
        app.activate();
        app.start_new_session(None);
        app.input = "/model opus".into();
        app.activate();
        assert_eq!(app.launch_model.as_deref(), Some("opus"));

        app.start_new_session(None);
        app.input = "ship it".into();
        assert_eq!(
            app.activate(),
            AppAction::Launch {
                provider: Provider::Claude,
                model: Some("opus".into()),
                prompt: "ship it".into(),
                cwd: None,
            }
        );

        app.start_new_session(None);
        app.input = "/filter codex".into();
        assert_eq!(app.activate(), AppAction::None);
        assert_eq!(app.filter, "codex");

        app.start_new_session(None);
        app.input = "/help".into();
        assert_eq!(app.activate(), AppAction::None);
        assert_eq!(app.overlay, Overlay::Help);
        assert_eq!(app.notice, None);

        app.start_new_session(None);
        app.input = "/setup copilot".into();
        assert_eq!(
            app.activate(),
            AppAction::SetupProvider {
                provider: Provider::GitHubCopilot
            }
        );

        app.start_new_session(None);
        app.input = "/setup shell".into();
        assert_eq!(
            app.activate(),
            AppAction::SetupProvider {
                provider: Provider::Terminal
            }
        );

        for (name, provider) in [
            ("vibe", Provider::MistralVibe),
            ("muse", Provider::MuseCode),
            ("qwen", Provider::QwenCode),
            ("kimi", Provider::KimiCode),
            ("omp", Provider::OhMyPi),
            ("oh-my-pi", Provider::OhMyPi),
            ("grok", Provider::Grok),
            ("kilo", Provider::KiloCode),
            ("openhands", Provider::OpenHands),
        ] {
            app.start_new_session(None);
            app.input = format!("/setup {name}");
            assert_eq!(
                app.activate(),
                AppAction::SetupProvider { provider },
                "setup alias {name} did not resolve"
            );
        }
    }

    #[test]
    fn terminal_shell_command_aliases_model_and_install_rows_are_explicit_actions() {
        let mut app = App::with_launch_targets(
            SessionSnapshot::default(),
            false,
            Provider::Terminal,
            vec![LaunchTarget {
                provider: Provider::Terminal,
                supports_model: true,
            }],
        );

        app.start_new_session(None);
        app.input = "/shell".into();
        assert_eq!(
            app.activate(),
            AppAction::LoadModels {
                provider: Provider::Terminal
            }
        );
        let (shell, install) = if cfg!(windows) {
            ("cmd", "install-shell:nu")
        } else {
            ("bash", "install-shell:fish")
        };
        app.set_available_models(Provider::Terminal, Ok(vec![shell.into(), install.into()]));
        app.model_selection = 2;
        assert_eq!(
            app.activate(),
            AppAction::SetupLaunchOption {
                provider: Provider::Terminal,
                option: install.into(),
            }
        );
        assert_eq!(app.overlay, Overlay::ModelPicker);
        assert_eq!(app.launch_model, None);

        app.model_selection = 1;
        assert_eq!(app.activate(), AppAction::None);
        assert_eq!(app.launch_model.as_deref(), Some(shell));

        app.start_new_session(None);
        app.input = "/model zsh".into();
        assert_eq!(app.activate(), AppAction::None);
        assert_eq!(app.launch_model.as_deref(), Some("zsh"));
    }

    #[test]
    fn shell_command_is_scoped_to_terminal() {
        let mut app = App::new(SessionSnapshot::default());
        app.start_new_session(None);
        app.input = "/shell".into();
        assert_eq!(app.activate(), AppAction::None);
        assert_eq!(
            app.notice.as_deref(),
            Some("/shell is available after selecting Terminal")
        );
    }

    #[test]
    fn harness_picker_previews_wraps_confirms_and_preserves_the_draft() {
        let mut app = App::with_launch_targets(
            SessionSnapshot::default(),
            false,
            Provider::Claude,
            vec![
                LaunchTarget {
                    provider: Provider::Claude,
                    supports_model: true,
                },
                LaunchTarget {
                    provider: Provider::Codex,
                    supports_model: true,
                },
                LaunchTarget {
                    provider: Provider::Pi,
                    supports_model: false,
                },
            ],
        );
        app.start_new_session(None);
        app.input = "keep this draft".into();
        app.launch_model = Some("opus".into());

        app.open_harness_picker();
        assert_eq!(app.overlay, Overlay::HarnessPicker);
        app.move_harness_selection(-1);
        assert_eq!(app.harness_selection, 2);
        assert_eq!(app.launch_provider, Provider::Claude);
        assert_eq!(app.escape(), AppAction::None);
        assert_eq!(app.overlay, Overlay::Composer(ComposerMode::NewSession));
        assert_eq!(app.input, "keep this draft");
        assert_eq!(app.launch_model.as_deref(), Some("opus"));

        app.open_harness_picker();
        app.move_harness_selection(1);
        assert_eq!(app.activate(), AppAction::None);
        assert_eq!(app.overlay, Overlay::Composer(ComposerMode::NewSession));
        assert_eq!(app.launch_provider, Provider::Codex);
        assert!(app.launch_model.is_none());
        assert_eq!(app.input, "keep this draft");

        app.open_harness_picker();
        app.choose_harness_number(3);
        assert_eq!(app.launch_provider, Provider::Pi);
        assert_eq!(app.overlay, Overlay::Composer(ComposerMode::NewSession));
    }

    #[test]
    fn provider_refresh_does_not_close_the_harness_picker_or_drop_its_draft() {
        let mut app = App::with_launch_targets(
            SessionSnapshot::default(),
            false,
            Provider::Claude,
            vec![
                LaunchTarget {
                    provider: Provider::Claude,
                    supports_model: true,
                },
                LaunchTarget {
                    provider: Provider::Pi,
                    supports_model: false,
                },
            ],
        );
        app.start_new_session(None);
        app.input = "draft during discovery".into();
        app.open_harness_picker();

        app.replace_snapshot(SessionSnapshot {
            sessions: vec![session("arrived", SessionState::Working)],
            warnings: vec![],
        });

        assert_eq!(app.overlay, Overlay::HarnessPicker);
        assert_eq!(app.input, "draft during discovery");
        assert_eq!(app.selection, Some(SelectionKey::Session("arrived".into())));
    }

    #[test]
    fn task_command_validation_is_local_and_harness_switch_resets_model() {
        let mut app = App::with_launch_targets(
            SessionSnapshot::default(),
            false,
            Provider::Claude,
            vec![
                LaunchTarget {
                    provider: Provider::Claude,
                    supports_model: true,
                },
                LaunchTarget {
                    provider: Provider::Codex,
                    supports_model: true,
                },
            ],
        );
        app.launch_model = Some("opus".into());
        app.start_new_session(None);
        app.open_harness_picker();
        app.move_harness_selection(1);
        app.activate();
        assert_eq!(app.launch_provider, Provider::Codex);
        assert!(app.launch_model.is_none());

        app.start_new_session(None);
        app.input = "/model invalid model".into();
        app.activate();
        assert!(app.launch_model.is_none());
        assert!(app
            .notice
            .as_deref()
            .unwrap()
            .contains("without whitespace"));

        app.start_new_session(None);
        app.input = "/unknown".into();
        app.activate();
        assert!(app
            .notice
            .as_deref()
            .unwrap()
            .contains("unknown dashboard command"));
    }

    #[test]
    fn completed_command_requests_toggle_or_explicit_visibility_without_guessing() {
        let mut app = App::with_completed_visibility(SessionSnapshot::default(), false);

        app.start_new_session(None);
        app.input = "/completed".into();
        assert_eq!(
            app.activate(),
            AppAction::SetCompletedVisibility {
                include_completed: true
            }
        );
        assert!(!app.includes_completed);

        app.set_completed_visibility(true);
        assert!(app.includes_completed);
        assert_eq!(app.notice.as_deref(), Some("showing completed sessions"));

        app.start_new_session(None);
        app.input = "/completed hide".into();
        assert_eq!(
            app.activate(),
            AppAction::SetCompletedVisibility {
                include_completed: false
            }
        );

        app.start_new_session(None);
        app.input = "/completed sometimes".into();
        assert_eq!(app.activate(), AppAction::None);
        assert!(app.notice.as_deref().unwrap().contains("/completed show"));
    }

    #[test]
    fn model_picker_loads_filters_pages_and_preserves_the_task_draft() {
        let mut app = App::with_launch_targets(
            SessionSnapshot::default(),
            false,
            Provider::Pi,
            vec![LaunchTarget {
                provider: Provider::Pi,
                supports_model: true,
            }],
        );
        app.start_new_session(None);
        app.input = "keep this task draft".into();

        assert_eq!(
            app.open_model_picker(),
            AppAction::LoadModels {
                provider: Provider::Pi
            }
        );
        assert_eq!(app.overlay, Overlay::ModelPicker);
        assert_eq!(app.input, "keep this task draft");
        assert!(app.models_loading);
        assert_eq!(app.model_choices(), vec![None]);

        app.set_available_models(
            Provider::Pi,
            Ok(vec![
                "openai/gpt-5".into(),
                "anthropic/claude-sonnet".into(),
                "openai/gpt-5".into(),
                "invalid model".into(),
            ]),
        );
        assert!(!app.models_loading);
        assert_eq!(
            app.model_choices(),
            vec![None, Some("openai/gpt-5"), Some("anthropic/claude-sonnet")]
        );

        app.push_input('g');
        app.push_input('p');
        app.push_input('t');
        assert_eq!(app.model_choices(), vec![Some("openai/gpt-5")]);
        assert_eq!(app.model_selection, 0);
        app.confirm_model_selection();
        assert_eq!(app.launch_model.as_deref(), Some("openai/gpt-5"));
        assert_eq!(app.overlay, Overlay::Composer(ComposerMode::NewSession));
        assert_eq!(app.input, "keep this task draft");

        app.open_model_picker();
        assert_eq!(app.model_selection, 1);
        app.move_model_selection(-1);
        assert_eq!(app.model_selection, 0);
        app.move_model_selection(-1);
        assert_eq!(app.model_selection, 2);
        app.move_model_page(-1);
        assert_eq!(app.model_selection, 0);
        assert_eq!(app.escape(), AppAction::None);
        assert_eq!(app.overlay, Overlay::Composer(ComposerMode::NewSession));
        assert_eq!(app.input, "keep this task draft");
    }

    #[test]
    fn model_picker_ignores_stale_provider_results_and_surfaces_current_errors() {
        let mut app = App::new(SessionSnapshot::default());
        app.start_new_session(None);
        app.open_model_picker();

        app.set_available_models(Provider::Codex, Ok(vec!["stale".into()]));
        assert!(app.available_models.is_empty());
        assert!(app.models_loading);

        app.set_available_models(Provider::Claude, Err("command timed out".into()));
        assert!(!app.models_loading);
        assert!(app.model_choices().is_empty());
        assert_eq!(app.notice, None);
        assert_eq!(app.models_error.as_deref(), Some("command timed out"));

        app.set_models_auth_available(&Provider::Claude, true);
        assert_eq!(
            app.activate(),
            AppAction::Authenticate {
                provider: Provider::Claude
            }
        );
        app.retry_model_load(&Provider::Claude);
        assert!(app.models_loading);
        assert!(app.models_error.is_none());
    }

    #[test]
    fn launch_authentication_failure_preserves_the_task_and_makes_enter_actionable() {
        let mut app = App::new(SessionSnapshot::default());
        app.require_authentication(
            Provider::GitHubCopilot,
            Some("gpt-5.4".into()),
            "finish the release".into(),
            "GitHub Copilot is not authenticated".into(),
        );

        assert_eq!(app.overlay, Overlay::ModelPicker);
        assert_eq!(app.input, "finish the release");
        assert_eq!(app.launch_model.as_deref(), Some("gpt-5.4"));
        assert_eq!(
            app.activate(),
            AppAction::Authenticate {
                provider: Provider::GitHubCopilot
            }
        );
        assert_eq!(app.input, "finish the release");
    }

    #[test]
    fn exact_model_id_is_an_explicit_fallback_when_catalog_loading_fails() {
        let mut app = App::new(SessionSnapshot::default());
        app.require_authentication(
            Provider::Claude,
            None,
            "investigate the failure".into(),
            "model catalog timed out".into(),
        );
        for character in "gemini-3-pro".chars() {
            app.push_input(character);
        }
        assert!(app.has_valid_custom_model_input());
        assert_eq!(app.activate(), AppAction::None);
        assert_eq!(app.launch_model.as_deref(), Some("gemini-3-pro"));
        assert_eq!(app.input, "investigate the failure");
        assert_eq!(app.overlay, Overlay::Composer(ComposerMode::NewSession));
    }

    #[test]
    fn exact_model_id_is_available_when_provider_has_no_machine_readable_catalog() {
        let mut app = App::with_launch_targets(
            SessionSnapshot::default(),
            false,
            Provider::QwenCode,
            vec![LaunchTarget {
                provider: Provider::QwenCode,
                supports_model: true,
            }],
        );
        app.start_new_session(None);
        app.input = "keep the native task".into();
        assert_eq!(
            app.open_model_picker(),
            AppAction::LoadModels {
                provider: Provider::QwenCode
            }
        );
        app.set_available_models(Provider::QwenCode, Ok(Vec::new()));
        for character in "qwen3-coder-plus".chars() {
            app.push_input(character);
        }

        assert!(app.has_valid_custom_model_input());
        app.confirm_model_selection();
        assert_eq!(app.launch_model.as_deref(), Some("qwen3-coder-plus"));
        assert_eq!(app.input, "keep the native task");
    }

    #[test]
    fn antigravity_catalog_failure_keeps_enter_on_native_recovery() {
        let mut app = App::new(SessionSnapshot::default());
        app.require_authentication(
            Provider::Antigravity,
            None,
            "investigate the failure".into(),
            "agy models timed out".into(),
        );
        for character in "gemini".chars() {
            app.push_input(character);
        }

        assert!(!app.has_valid_custom_model_input());
        assert_eq!(
            app.activate(),
            AppAction::Authenticate {
                provider: Provider::Antigravity
            }
        );
        assert_eq!(app.input, "investigate the failure");
        assert_eq!(app.launch_model, None);
    }
}
