//! The add-connection panel: the form plus saving and testing, in one value an app keeps while the
//! panel is open.
//!
//! ```ignore
//! // when ai_status reports StatusAction::AddConnection:
//! panel = Some(ConnectionPanel::new(ConnectionManager::standard()?));
//! // every frame while it is open:
//! if let Some(p) = &mut panel {
//!     match p.show(ui) {
//!         Some(PanelEvent::Saved(_)) | Some(PanelEvent::Closed) => panel = None,
//!         None => {}
//!     }
//! }
//! ```
//!
//! It writes through [`ConnectionManager`], the same code the launcher's connections screen uses,
//! so a connection added here appears in the launcher and in every other Craft app, and the other
//! way round.

use std::time::Duration;

use craft_ai_client::{ConnectionManager, SaveError, TestRunner, TestTarget};
use craft_ai_core::{ConnectionId, DraftError, ErrorKind, Field};
use egui::Ui;

use crate::form::{ConnectionFormState, FormEvent, TestLine, connection_form};

/// What happened in the panel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PanelEvent {
    /// The connection was saved. The app should look at its status again.
    Saved(ConnectionId),
    /// Cancelled.
    Closed,
}

/// The panel's state.
pub struct ConnectionPanel {
    manager: ConnectionManager,
    runner: TestRunner,
    form: ConnectionFormState,
    /// The address the running test was started against, so an answer arriving after the address
    /// changed can be told from one that still describes it. `None` while no test is running.
    testing: Option<String>,
}

impl ConnectionPanel {
    pub fn new(manager: ConnectionManager) -> Self {
        Self::with_form(manager, ConnectionFormState::new())
    }

    /// Starts from a given form, to edit an existing connection.
    pub fn with_form(manager: ConnectionManager, form: ConnectionFormState) -> Self {
        Self {
            runner: TestRunner::new(manager.clone()),
            manager,
            form,
            testing: None,
        }
    }

    /// Draws the panel and handles what was pressed.
    pub fn show(&mut self, ui: &mut Ui) -> Option<PanelEvent> {
        if self.testing.is_some()
            && let Some(answer) = self.runner.poll()
            && let Some(tested) = self.testing.take()
        {
            // The address stays editable while a test runs, so this answer may describe an
            // address the form no longer holds. Editing already cleared the models and the
            // result line, leaving the form as untested; taking the answer now would put one
            // endpoint's models under another, so it is dropped instead.
            if tested.trim() == self.form.draft.endpoint.trim() {
                match answer {
                    Ok(report) => {
                        self.form.test = TestLine::Passed {
                            models: report.models.len(),
                            millis: u64::try_from(report.elapsed.as_millis()).unwrap_or(u64::MAX),
                        };
                        self.form.draft.set_models(report.models);
                    }
                    Err(e)
                        if e.kind == ErrorKind::Unsupported
                            && self.form.draft.provider == "vertex-express" =>
                    {
                        self.form.test = TestLine::Unverified(e.message);
                    }
                    Err(e) => self.form.test = TestLine::Failed(e.message),
                }
            }
        }
        if self.testing.is_some() {
            ui.ctx().request_repaint_after(Duration::from_millis(100));
        }
        match connection_form(ui, &mut self.form, self.testing.is_some())? {
            FormEvent::Cancel => Some(PanelEvent::Closed),
            FormEvent::Test => {
                self.form.test = TestLine::Running;
                self.form.error = None;
                self.testing = Some(self.form.draft.endpoint.trim().to_owned());
                self.runner
                    .start(TestTarget::Draft(self.form.draft.clone()));
                None
            }
            FormEvent::Save => match self.manager.save(&self.form.draft) {
                Ok(saved) => Some(PanelEvent::Saved(saved.id)),
                Err(SaveError::Invalid(e)) => {
                    self.form.error = Some(e);
                    None
                }
                Err(SaveError::Failed(e)) => {
                    self.form.error = Some(DraftError {
                        field: Field::Label,
                        message: e.message,
                    });
                    None
                }
            },
        }
    }

    /// The form, for tests and for apps that want to look.
    pub fn form(&self) -> &ConnectionFormState {
        &self.form
    }
}
