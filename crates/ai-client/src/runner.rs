//! Running the Test button without freezing a screen.
//!
//! A test may wait on a slow or dead server, so it runs on its own thread and the screen polls for
//! the answer each frame. Only one answer is wanted at a time: starting a new test makes an older
//! one, if it arrives late, disappear, so a person who edited the form and pressed Test again never
//! sees the previous result.

use std::sync::mpsc::{Receiver, Sender, channel};

use craft_ai_core::{AiError, ConnectionDraft, ConnectionId};

use crate::manage::{ConnectionManager, TestReport};

/// What to test.
///
/// One value is made per click and moved to a worker thread, so the size gap between the variants
/// costs nothing. Boxing the draft would change a public type its callers match on.
#[derive(Clone, Debug)]
#[allow(clippy::large_enum_variant)]
pub enum TestTarget {
    /// The form as typed, before anything is saved.
    Draft(ConnectionDraft),
    /// A saved connection. The models found replace its saved list.
    Saved(ConnectionId),
}

type Answer = (u64, Result<TestReport, AiError>);

/// Starts tests in the background and hands back the answer to the latest one.
pub struct TestRunner {
    manager: ConnectionManager,
    tx: Sender<Answer>,
    rx: Receiver<Answer>,
    generation: u64,
}

impl TestRunner {
    pub fn new(manager: ConnectionManager) -> Self {
        let (tx, rx) = channel();
        Self {
            manager,
            tx,
            rx,
            generation: 0,
        }
    }

    /// Starts a test, replacing any earlier one.
    pub fn start(&mut self, target: TestTarget) {
        self.generation += 1;
        let generation = self.generation;
        let tx = self.tx.clone();
        let manager = self.manager.clone();
        std::thread::spawn(move || {
            let result = match target {
                TestTarget::Draft(draft) => manager.test_draft(&draft),
                TestTarget::Saved(id) => manager.test_saved(&id, true),
            };
            // The screen may have gone; nobody is waiting then.
            let _ = tx.send((generation, result));
        });
    }

    /// The answer to the latest test, once it arrives. Older answers are thrown away.
    pub fn poll(&mut self) -> Option<Result<TestReport, AiError>> {
        while let Ok((generation, answer)) = self.rx.try_recv() {
            if generation == self.generation {
                return Some(answer);
            }
        }
        None
    }

    /// The manager the tests run through.
    pub fn manager(&self) -> &ConnectionManager {
        &self.manager
    }
}
