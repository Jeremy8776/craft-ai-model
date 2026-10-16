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
    testing: bool,
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
            testing: false,
        }
    }

    /// Draws the panel and handles what was pressed.
    pub fn show(&mut self, ui: &mut Ui) -> Option<PanelEvent> {
        if self.testing
            && let Some(answer) = self.runner.poll()
        {
            self.testing = false;
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
        if self.testing {
            ui.ctx().request_repaint_after(Duration::from_millis(100));
        }
        match connection_form(ui, &mut self.form, self.testing)? {
            FormEvent::Cancel => Some(PanelEvent::Closed),
            FormEvent::Test => {
                self.form.test = TestLine::Running;
                self.form.error = None;
                self.testing = true;
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
