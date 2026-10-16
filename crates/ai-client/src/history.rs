//! Writing the request history from [`AiClient`](crate::AiClient).
//!
//! `start` writes the start line, and the first poll that sees a final state writes the end line
//! (once, however many clones of the job poll). A write that fails is dropped: the history is a
//! convenience and must never turn a working job into a failed one.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use craft_ai_core::history::{HISTORY_SCHEMA, clip_prompt};
use craft_ai_core::{
    AiError, CostEstimate, EndRecord, ErrorKind, HistoryRecord, InputRole, JobId, JobState,
    Outcome, Payload, StartRecord, Usage,
};
use craft_ai_native::HistoryFile;

use crate::client::Plan;

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Unix milliseconds now; 0 if the clock is before 1970.
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| u64::try_from(d.as_millis()).ok())
        .unwrap_or(0)
}

/// Unique across processes (time and process id) and within one (counter).
fn new_run_id(at_ms: u64) -> String {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{at_ms:x}-{:x}-{n:x}", std::process::id())
}

fn prompt_text(plan: &Plan) -> Option<String> {
    let parts: Vec<&str> = plan
        .request
        .inputs
        .iter()
        .filter(|i| i.role == InputRole::Prompt)
        .filter_map(|i| match &i.payload {
            Payload::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    (!parts.is_empty()).then(|| clip_prompt(&parts.join("\n\n")))
}

fn start_record(
    plan: &Plan,
    provider_job: Option<&JobId>,
    estimate: Option<CostEstimate>,
    keep_prompts: bool,
) -> StartRecord {
    let at_ms = now_ms();
    StartRecord {
        v: HISTORY_SCHEMA,
        run: new_run_id(at_ms),
        at_ms,
        app: plan.disclosure.app.to_string(),
        role_id: plan.role_id.to_string(),
        role_label: plan.role_label.clone(),
        capability: plan.request.capability.to_string(),
        connection: plan.connection.id.to_string(),
        connection_label: plan.connection.label.clone(),
        model: plan.model.to_string(),
        locality: plan.disclosure.locality,
        destination: plan.disclosure.destination.clone(),
        sent: plan.disclosure.items.clone(),
        params: plan.disclosure.param_names.clone(),
        provider_job: provider_job.map(ToString::to_string),
        estimate,
        prompt: if keep_prompts {
            prompt_text(plan)
        } else {
            None
        },
    }
}

/// Writes the end line for one run, once.
pub(crate) struct Recorder {
    file: Arc<HistoryFile>,
    run: String,
    done: AtomicBool,
    /// The latest usage any poll reported, in case the final snapshot carries none.
    usage: Mutex<Option<Usage>>,
}

impl Recorder {
    fn new(file: Arc<HistoryFile>, run: String) -> Self {
        Self {
            file,
            run,
            done: AtomicBool::new(false),
            usage: Mutex::new(None),
        }
    }

    /// Called with every snapshot; writes the end line when the state is final.
    pub(crate) fn observe(&self, state: &JobState, usage: Option<&Usage>) {
        if let Some(u) = usage
            && let Ok(mut last) = self.usage.lock()
        {
            *last = Some(u.clone());
        }
        let outcome = match state {
            JobState::Succeeded => Outcome::Done,
            JobState::Failed { error } => Outcome::Failed { kind: error.kind },
            JobState::Cancelled => Outcome::Cancelled,
            JobState::Queued | JobState::Running { .. } => return,
        };
        self.finish(outcome);
    }

    /// The caller stopped waiting and cancelled the job.
    pub(crate) fn gave_up(&self) {
        self.finish(Outcome::Failed {
            kind: ErrorKind::Network,
        });
    }

    fn finish(&self, outcome: Outcome) {
        if self.done.swap(true, Ordering::SeqCst) {
            return;
        }
        let usage = self.usage.lock().ok().and_then(|u| u.clone());
        let _ = self.file.append(&HistoryRecord::End(EndRecord {
            v: HISTORY_SCHEMA,
            run: self.run.clone(),
            at_ms: now_ms(),
            outcome,
            usage,
        }));
    }
}

/// The job is submitted: writes its start line.
pub(crate) fn begin(
    file: &Arc<HistoryFile>,
    plan: &Plan,
    provider_job: &JobId,
    estimate: Option<CostEstimate>,
    keep_prompts: bool,
) -> Recorder {
    let record = start_record(plan, Some(provider_job), estimate, keep_prompts);
    let run = record.run.clone();
    let _ = file.append(&HistoryRecord::Start(record));
    Recorder::new(Arc::clone(file), run)
}

/// The provider (or the key, or the adapter) refused before a job existed: a start and an end.
pub(crate) fn refused(
    file: &Arc<HistoryFile>,
    plan: &Plan,
    estimate: Option<CostEstimate>,
    keep_prompts: bool,
    error: &AiError,
) {
    let record = start_record(plan, None, estimate, keep_prompts);
    let run = record.run.clone();
    let _ = file.append(&HistoryRecord::Start(record));
    Recorder::new(Arc::clone(file), run).finish(Outcome::Failed { kind: error.kind });
}
