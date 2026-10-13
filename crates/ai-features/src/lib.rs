//! What the Craft apps can do with AI, and what a computer still needs to do it.
//!
//! The launcher shows [`Feature`]s, not providers. Each feature names the tools it powers in the
//! apps, a local recipe (model files and node packs for ComfyUI) where one has been worked out,
//! and hosted options that have an adapter. [`plan`] compares a recipe with an [`Inventory`] of
//! what the machine has and says what is left: ready, a download of a stated size, a licence to
//! accept, or a reason it cannot run.
//!
//! Pure data and logic, serde only, so it compiles for wasm. Scanning the disk and downloading
//! belong to the native side.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod catalogue;
mod engine;
mod feature;
mod options;
mod plan;
mod png;
mod recipe;
mod sample;
mod setup;

pub use catalogue::builtin;
pub use engine::{
    EngineAdvice, EngineOffer, Gpu, GpuVendor, Os, Pace, SystemProfile, advise, gpu_route,
};
pub use feature::{AppStatus, AppUse, Feature};
pub use options::{Route, SetupOption, setup_options};
pub use plan::{Blocker, InstalledFile, Inventory, Plan, State, plan};
pub use recipe::{CloudOption, Licence, LocalRecipe, ModelFile, NativeRoute, NodePack, Source};
pub use sample::{sample_is_costly, sample_request};
pub use setup::{
    Agreement, COMFYUI_TAG, PortableBuild, SetupOffer, SetupRoute, SetupStep, StepKind, agreement,
    gigabytes, is_official_build, portable_for, setup_offer,
};
