//! The app-facing entry point of the Craft AI layer.
//!
//! An app declares its [`Role`](craft_ai_core::Role)s, builds one [`AiClient`], and for each
//! feature runs: `status` (can it run?), `plan` (what would be sent?), consent if needed, `start`
//! (go), then polls the [`Job`]. Everything underneath (the launcher's shared store, the key
//! stores, the provider adapters) is reached through that one type.
//!
//! ```no_run
//! use craft_ai_client::AiClient;
//! use craft_ai_core::{AppId, CapabilityId, ConsentLedger, InputRole, Payload, Request, Role, RoleId};
//!
//! let client = AiClient::standard(AppId::new("wordcraft")?)?;
//! let rewrite = Role {
//!     id: RoleId::new("rewrite")?,
//!     label: "Rewrite".into(),
//!     capability: CapabilityId::parse("text.complete")?,
//!     inputs: vec![InputRole::Prompt],
//! };
//! if let craft_ai_client::RoleStatus::Unavailable { reason } = client.status(&rewrite) {
//!     println!("Rewrite is off: {reason}");
//!     return Ok(());
//! }
//! let request = Request::new(rewrite.capability.clone())
//!     .with_input(InputRole::Prompt, Payload::text("Tighten this paragraph."));
//! let plan = client.plan(&rewrite, request)?;
//! let ledger = ConsentLedger::default(); // load the app's saved ledger instead
//! if plan.needs_consent(&ledger) {
//!     // show plan.disclosure(), then ledger.grant(plan.disclosure()) if the user agrees
//! }
//! let job = client.start(&plan, &ledger)?;
//! # let _ = job;
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unimplemented,
    clippy::todo,
    clippy::unreachable
)]

mod client;
mod declarative;
mod engine;
mod factory;
mod history;
mod local;
mod manage;
mod picker;
mod picker_groups;
mod popup;
mod runner;
mod tools;

pub use client::{AiClient, Choice, Finished, Job, PickedChoice, Plan, RoleReady, RoleStatus};
pub use factory::{BuildFn, ProviderFactory};
pub use local::{LocalEnabled, LocalRoute};
pub use manage::{
    Confirmed, ConnectionManager, ConnectionView, KeyState, MAX_DECLARED, ModelChoice, ProbeReport,
    ProbedModel, Removed, SaveError, Saved, TestReport, Unconfirmed, declare_models,
};
pub use picker::{CurrentChoice, GroupState, PickerGroup, PickerModel, PickerView, SetupLink};
pub use popup::MissingModel;
pub use runner::{TestRunner, TestTarget};
pub use tools::{Accepted, SampleReport, Skipped, TestStart, ToolDesk, ToolView};
