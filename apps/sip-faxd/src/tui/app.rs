//! The state of the TUI and what the keys and the live events do to it. No terminal and no
//! socket in here: the data comes from a [`FaxStore`] and from [`Live`] messages that the shell
//! (`super::run`) reads off the control socket.

use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use store::{DeliveryAttempt, Error, FaxRecord, FaxStore, ListFilter, PageRequest};

use super::i18n::{Key, Lang, t};
use crate::service::protocol::{CallInfo, Event, StatusInfo};

/// How many faxes the list loads.
const LIST_LIMIT: u32 = 200;

/// The three panes `Tab` goes through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    /// The received faxes (and their details).
    Faxes,
    /// The calls in progress.
    Calls,
    /// The state of the service.
    Status,
}

/// One fax opened with `Enter`.
#[derive(Debug, Clone)]
pub struct Detail {
    /// The record.
    pub fax: FaxRecord,
    /// Its delivery attempts.
    pub deliveries: Vec<DeliveryAttempt>,
}

/// A note being typed.
#[derive(Debug, Clone)]
pub struct NoteEdit {
    /// The fax.
    pub fax_id: i64,
    /// The text so far.
    pub text: String,
    /// `note_updated_at` of the record when the editing began (the optimistic lock).
    pub expected: Option<chrono::DateTime<chrono::Utc>>,
}

/// What the control socket tells the TUI.
#[derive(Debug, Clone)]
pub enum Live {
    /// The subscription is open; these calls are running.
    Connected(Vec<CallInfo>),
    /// A streamed event.
    Event(Event),
    /// The status answer.
    Status(StatusInfo),
    /// The socket is gone or was never there.
    Down,
}

/// What a key asks for besides changing the state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Nothing more to do.
    Nothing,
    /// Load the fax list again.
    Reload,
    /// Open the selected fax.
    OpenDetail,
    /// Toggle the read flag of the selected fax.
    ToggleRead,
    /// Save the note being edited.
    SaveNote,
}

/// The whole state.
pub struct App {
    store: Option<Arc<dyn FaxStore>>,
    operator: String,
    /// Interface language.
    pub lang: Lang,
    /// The pane shown.
    pub pane: Pane,
    /// Only unread faxes.
    pub unread_only: bool,
    /// The loaded faxes, newest first.
    pub faxes: Vec<FaxRecord>,
    /// Index into `faxes`.
    pub fax_sel: usize,
    /// The fax opened (the details screen).
    pub detail: Option<Detail>,
    /// The note being typed.
    pub note: Option<NoteEdit>,
    /// The calls in progress.
    pub calls: Vec<CallInfo>,
    /// Index into `calls`.
    pub call_sel: usize,
    /// `None` while the service is not reachable.
    pub status: Option<StatusInfo>,
    /// Whether the control socket is connected.
    pub live: bool,
    /// The last message (shown in the footer).
    pub message: Option<String>,
    /// `q` was pressed.
    pub quit: bool,
}

impl App {
    /// A TUI over `store` (`None` when the database cannot be reached); changes are recorded for
    /// `operator`.
    pub fn new(store: Option<Arc<dyn FaxStore>>, operator: String, lang: Lang) -> App {
        App {
            store,
            operator,
            lang,
            pane: Pane::Faxes,
            unread_only: false,
            faxes: Vec::new(),
            fax_sel: 0,
            detail: None,
            note: None,
            calls: Vec::new(),
            call_sel: 0,
            status: None,
            live: false,
            message: None,
            quit: false,
        }
    }

    /// Whether the database is connected.
    pub fn has_store(&self) -> bool {
        self.store.is_some()
    }

    /// Hands over a store connected later.
    pub fn set_store(&mut self, store: Arc<dyn FaxStore>) {
        self.store = Some(store);
    }

    fn say(&mut self, key: Key) {
        self.message = Some(t(self.lang, key).to_string());
    }

    fn say_error(&mut self, e: impl std::fmt::Display) {
        self.message = Some(format!("{}: {e}", t(self.lang, Key::MsgError)));
    }

    /// The fax under the cursor.
    pub fn selected_fax(&self) -> Option<&FaxRecord> {
        self.faxes.get(self.fax_sel)
    }

    // ------------------------------------------------------------------------ data

    /// Loads the list again (keeping the cursor on the same fax when it is still there) and
    /// refreshes the open details.
    pub async fn reload(&mut self) {
        let Some(store) = self.store.clone() else {
            return;
        };
        let keep = self.selected_fax().map(|f| f.id);
        let filter = ListFilter {
            is_read: self.unread_only.then_some(false),
            ..ListFilter::default()
        };
        match store
            .list(
                &filter,
                PageRequest {
                    cursor: None,
                    limit: LIST_LIMIT,
                },
            )
            .await
        {
            Ok(page) => {
                self.faxes = page.items;
                self.fax_sel = keep
                    .and_then(|id| self.faxes.iter().position(|f| f.id == id))
                    .unwrap_or(self.fax_sel)
                    .min(self.faxes.len().saturating_sub(1));
            }
            Err(e) => self.say_error(e),
        }
        if let Some(id) = self.detail.as_ref().map(|d| d.fax.id) {
            self.load_detail(id).await;
        }
    }

    async fn load_detail(&mut self, id: i64) {
        let Some(store) = self.store.clone() else {
            return;
        };
        let fax = match store.get(id).await {
            Ok(Some(f)) => f,
            // purged meanwhile
            Ok(None) => {
                self.detail = None;
                return;
            }
            Err(e) => {
                self.say_error(e);
                return;
            }
        };
        let deliveries = store.list_deliveries(id).await.unwrap_or_default();
        self.detail = Some(Detail { fax, deliveries });
    }

    /// The fax the keys `r` and `n` act on: the opened one, else the one under the cursor.
    fn target(&self) -> Option<&FaxRecord> {
        self.detail
            .as_ref()
            .map(|d| &d.fax)
            .or_else(|| self.selected_fax())
    }

    async fn toggle_read(&mut self) {
        let Some(store) = self.store.clone() else {
            return;
        };
        let Some((id, now_read)) = self.target().map(|f| (f.id, f.is_read)) else {
            self.say(Key::MsgNoSelection);
            return;
        };
        match store.set_read(id, !now_read, &self.operator).await {
            Ok(_) => {
                self.say(if now_read {
                    Key::MsgMarkedUnread
                } else {
                    Key::MsgMarkedRead
                });
                self.reload().await;
            }
            Err(e) => self.say_error(e),
        }
    }

    async fn save_note(&mut self) {
        let (Some(store), Some(edit)) = (self.store.clone(), self.note.take()) else {
            return;
        };
        match store
            .set_note(edit.fax_id, &edit.text, &self.operator, edit.expected)
            .await
        {
            Ok(_) => self.say(Key::MsgNoteSaved),
            Err(Error::Conflict(_)) => self.say(Key::MsgNoteConflict),
            Err(e) => self.say_error(e),
        }
        self.reload().await;
    }

    // ------------------------------------------------------------------------ live

    /// Applies a message from the control socket; refreshes the fax list when the event may
    /// have changed it.
    pub async fn apply_live(&mut self, live: Live) {
        match live {
            Live::Connected(calls) => {
                self.live = true;
                self.set_calls(calls);
                self.reload().await;
            }
            Live::Down => {
                self.live = false;
                self.status = None;
                self.calls.clear();
                self.call_sel = 0;
            }
            Live::Status(s) => {
                self.status = Some(s);
            }
            Live::Event(ev) => {
                if self.apply_event(ev) {
                    self.reload().await;
                }
            }
        }
    }

    fn set_calls(&mut self, calls: Vec<CallInfo>) {
        self.calls = calls;
        self.call_sel = self.call_sel.min(self.calls.len().saturating_sub(1));
    }

    /// Applies one event to the call list. Returns whether the fax list should be loaded again.
    pub fn apply_event(&mut self, ev: Event) -> bool {
        match ev {
            Event::CallStarted { call } => {
                self.calls.retain(|c| c.call_id != call.call_id);
                self.calls.push(call);
                true
            }
            Event::CallAnswered { call_id } => {
                if let Some(c) = self.call_mut(&call_id) {
                    c.answered = true;
                }
                false
            }
            Event::FaxStarted { call_id } => {
                if let Some(c) = self.call_mut(&call_id) {
                    c.t38 = true;
                }
                false
            }
            Event::FaxProgress { call_id, pages } => {
                if let Some(c) = self.call_mut(&call_id) {
                    c.pages = pages;
                }
                true
            }
            Event::CallFinished { call_id, .. } => {
                self.calls.retain(|c| c.call_id != call_id);
                self.call_sel = self.call_sel.min(self.calls.len().saturating_sub(1));
                true
            }
            Event::Stopping { active } => {
                if let Some(s) = self.status.as_mut() {
                    s.state = "stopping".into();
                    s.active_calls = active;
                }
                false
            }
        }
    }

    fn call_mut(&mut self, id: &str) -> Option<&mut CallInfo> {
        self.calls.iter_mut().find(|c| c.call_id == id)
    }

    // ------------------------------------------------------------------------ keys

    /// Handles a key and does what it asks (database access included).
    pub async fn on_key(&mut self, key: KeyEvent) {
        let action = self.handle_key(key);
        self.perform(action).await;
    }

    /// Carries out an [`Action`].
    pub async fn perform(&mut self, action: Action) {
        match action {
            Action::Nothing => {}
            Action::Reload => self.reload().await,
            Action::OpenDetail => {
                if let Some(id) = self.selected_fax().map(|f| f.id) {
                    self.load_detail(id).await;
                }
            }
            Action::ToggleRead => self.toggle_read().await,
            Action::SaveNote => self.save_note().await,
        }
    }

    /// Changes the state for a key; the returned action needs the database.
    pub fn handle_key(&mut self, key: KeyEvent) -> Action {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.quit = true;
            return Action::Nothing;
        }
        if self.note.is_some() {
            return self.note_key(key);
        }
        self.message = None;
        match key.code {
            KeyCode::Char('q') => {
                self.quit = true;
                Action::Nothing
            }
            KeyCode::Tab => {
                self.detail = None;
                self.pane = match self.pane {
                    Pane::Faxes => Pane::Calls,
                    Pane::Calls => Pane::Status,
                    Pane::Status => Pane::Faxes,
                };
                Action::Nothing
            }
            KeyCode::BackTab => {
                self.detail = None;
                self.pane = match self.pane {
                    Pane::Faxes => Pane::Status,
                    Pane::Calls => Pane::Faxes,
                    Pane::Status => Pane::Calls,
                };
                Action::Nothing
            }
            KeyCode::Esc => {
                self.detail = None;
                Action::Nothing
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.step(-1);
                Action::Nothing
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.step(1);
                Action::Nothing
            }
            KeyCode::Enter if self.pane == Pane::Faxes && self.detail.is_none() => {
                Action::OpenDetail
            }
            KeyCode::Char('u') if self.pane == Pane::Faxes && self.detail.is_none() => {
                self.unread_only = !self.unread_only;
                self.fax_sel = 0;
                Action::Reload
            }
            KeyCode::Char('r') if self.pane == Pane::Faxes => Action::ToggleRead,
            KeyCode::Char('n') if self.pane == Pane::Faxes => {
                match self
                    .target()
                    .map(|f| (f.id, f.note.clone(), f.note_updated_at))
                {
                    Some((fax_id, note, expected)) => {
                        self.note = Some(NoteEdit {
                            fax_id,
                            text: note.unwrap_or_default(),
                            expected,
                        });
                    }
                    None => self.say(Key::MsgNoSelection),
                }
                Action::Nothing
            }
            _ => Action::Nothing,
        }
    }

    fn note_key(&mut self, key: KeyEvent) -> Action {
        let Some(edit) = self.note.as_mut() else {
            return Action::Nothing;
        };
        match key.code {
            KeyCode::Esc => {
                self.note = None;
                Action::Nothing
            }
            KeyCode::Enter => Action::SaveNote,
            KeyCode::Backspace => {
                edit.text.pop();
                Action::Nothing
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                edit.text.push(c);
                Action::Nothing
            }
            _ => Action::Nothing,
        }
    }

    fn step(&mut self, delta: isize) {
        let (sel, len) = match self.pane {
            Pane::Faxes if self.detail.is_none() => (&mut self.fax_sel, self.faxes.len()),
            Pane::Calls => (&mut self.call_sel, self.calls.len()),
            _ => return,
        };
        if len == 0 {
            *sel = 0;
            return;
        }
        *sel = sel.saturating_add_signed(delta).min(len - 1);
    }
}
