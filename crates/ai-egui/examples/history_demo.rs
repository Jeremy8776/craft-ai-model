//! A window to look at the history view with sample data.
//!
//! ```text
//! cargo run -p craft-ai-egui --example history_demo -j 8            # sample data
//! cargo run -p craft-ai-egui --example history_demo -j 8 -- --real  # your actual jobs.jsonl
//! ```
//!
//! Nothing here deletes or writes the real log. Clear only empties what the window shows, and
//! Export writes `history-export.csv` into the current directory.

use std::time::{SystemTime, UNIX_EPOCH};

use craft_ai_core::history::{EndRecord, HISTORY_SCHEMA, StartRecord, merge};
use craft_ai_core::job::Usage;
use craft_ai_core::{ErrorKind, HistoryLog, HistoryRecord, Locality, Outcome};
use craft_ai_egui::{HistoryAction, HistoryViewState, history_view};
use craft_ai_native::HistoryFile;

const HOUR: u64 = 3_600_000;
/// UK summer time, for the demo. A real app supplies its own offset.
const OFFSET_MINUTES: i32 = 60;

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(0))
        .unwrap_or(0)
}

struct Sample {
    app: &'static str,
    role: &'static str,
    capability: &'static str,
    model: &'static str,
    connection: &'static str,
    remote: bool,
    cost: Option<u64>,
}

const SAMPLES: [Sample; 6] = [
    Sample {
        app: "photocraft",
        role: "Generative Fill",
        capability: "image.inpaint",
        model: "flux-fill-pro",
        connection: "Black Forest Labs",
        remote: true,
        cost: Some(50_000),
    },
    Sample {
        app: "photocraft",
        role: "Select subject",
        capability: "image.segment",
        model: "sam3",
        connection: "ComfyUI (this computer)",
        remote: false,
        cost: None,
    },
    Sample {
        app: "photocraft",
        role: "Generative Fill",
        capability: "image.inpaint",
        model: "gemini-2.5-flash-image",
        connection: "Vertex AI",
        remote: true,
        cost: Some(39_000),
    },
    Sample {
        app: "wordcraft",
        role: "Rewrite",
        capability: "text.complete",
        model: "gpt-4o-mini",
        connection: "OpenAI",
        remote: true,
        cost: Some(1_200),
    },
    Sample {
        app: "filmcraft",
        role: "Extend sequence",
        capability: "video.generate",
        model: "seedance-2.0",
        connection: "ArtCraft",
        remote: true,
        cost: Some(420_000),
    },
    Sample {
        app: "wordcraft",
        role: "Rewrite",
        capability: "text.complete",
        model: "llama3",
        connection: "Ollama (this computer)",
        remote: false,
        cost: None,
    },
];

fn sample_log(now: u64) -> HistoryLog {
    let mut records = Vec::new();
    for i in 0..60u64 {
        let s = &SAMPLES[usize::try_from(i * 7 + 3).unwrap_or(0) % SAMPLES.len()];
        // About three and a half hours apart, so 60 jobs span roughly nine days, with a few
        // minutes of jitter.
        let at = now.saturating_sub(i * 17 * HOUR / 5 + (i % 4) * 600_000);
        let run = format!("demo-{i}");
        records.push(HistoryRecord::Start(StartRecord {
            v: HISTORY_SCHEMA,
            run: run.clone(),
            at_ms: at,
            app: s.app.into(),
            role_id: s.role.to_lowercase().replace(' ', "-"),
            role_label: s.role.into(),
            capability: s.capability.into(),
            connection: "demo".into(),
            connection_label: s.connection.into(),
            model: s.model.into(),
            locality: if s.remote {
                Locality::Remote
            } else {
                Locality::Local
            },
            destination: "demo".into(),
            sent: Vec::new(),
            params: Vec::new(),
            provider_job: Some(format!("gen-{i:04}")),
            estimate: None,
            prompt: None,
        }));
        let outcome = match i % 11 {
            4 => Some(Outcome::Failed {
                kind: ErrorKind::Quota,
            }),
            7 => Some(Outcome::Failed {
                kind: ErrorKind::RateLimited,
            }),
            9 => Some(Outcome::Cancelled),
            0 if i > 0 => None,
            _ => Some(Outcome::Done),
        };
        if let Some(outcome) = outcome {
            let video = if s.capability == "video.generate" {
                70_000
            } else {
                0
            };
            let took = 1_500 + (i % 9) * 2_300 + video;
            let done = matches!(outcome, Outcome::Done);
            records.push(HistoryRecord::End(EndRecord {
                v: HISTORY_SCHEMA,
                run,
                at_ms: at + took,
                outcome,
                usage: s.cost.filter(|_| done).map(|c| Usage {
                    cost_micros: Some(c),
                    currency: Some("USD".into()),
                    ..Usage::default()
                }),
            }));
        }
    }
    merge(records, 0)
}

struct Demo {
    real: bool,
    log: HistoryLog,
    view: HistoryViewState,
    note: String,
}

impl Demo {
    fn load(real: bool) -> HistoryLog {
        if !real {
            return sample_log(now_ms());
        }
        match HistoryFile::at_default_location().map(|f| f.read()) {
            Some(Ok(read)) => merge(read.records, read.skipped),
            _ => HistoryLog::default(),
        }
    }
}

impl eframe::App for Demo {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ui, |ui| {
            ui.heading(if self.real {
                "AI history (your real log, read only)"
            } else {
                "AI history (sample data)"
            });
            ui.add_space(6.0);
            match history_view(ui, &mut self.view, &self.log, now_ms()) {
                Some(HistoryAction::Refresh) => {
                    self.log = Self::load(self.real);
                    self.note = "Refreshed.".into();
                }
                Some(HistoryAction::Clear) => {
                    self.log = HistoryLog::default();
                    self.note =
                        "Cleared in this window only. The file on disk was not touched.".into();
                }
                Some(HistoryAction::Export(csv)) => {
                    self.note = match std::fs::write("history-export.csv", csv) {
                        Ok(()) => "Wrote history-export.csv in the current folder.".into(),
                        Err(e) => format!("Could not write the file: {e}"),
                    };
                }
                None => {}
            }
            if !self.note.is_empty() {
                ui.add_space(6.0);
                ui.weak(&self.note);
            }
        });
    }
}

fn main() -> eframe::Result {
    let real = std::env::args().any(|a| a == "--real");
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([980.0, 560.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Craft AI history",
        options,
        Box::new(move |_cc| {
            Ok(Box::new(Demo {
                real,
                log: Demo::load(real),
                view: HistoryViewState::default().with_offset(OFFSET_MINUTES),
                note: String::new(),
            }))
        }),
    )
}
