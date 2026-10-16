//! Where a person may ask for a model file to come from, and the rules each place carries.
//!
//! This module group is the model-source registry. Everything here is checked before a
//! byte is fetched:
//!
//! - [`hosts`]: the only hosts each kind of source may use, including the storage hosts it
//!   redirects to, and the one host that may ever see a token.
//! - [`format`]: which file formats are installed. Pickle formats can run code when loaded, so
//!   they need explicit consent, and a file must be what its name says.
//! - [`spec`]: what a pasted address means. Anything else is refused, not guessed.

pub mod candidate;
pub mod format;
pub mod hosts;
pub mod install;
pub mod pack;
pub mod registry;
pub mod resolve;
pub mod settings;
pub mod spec;

pub use candidate::{Candidate, MAX_FILE_BYTES, Needs, licence_needs_accepting};
pub use format::{Format, FormatProblem};
pub use hosts::{HostRules, SourceKind, host_matches};
pub use install::{Consents, InstallError, Installer, Refusal};
pub use pack::{
    PackCandidate, PackConsent, PackEntry, PackError, PackLimits, PackRefusal, PackReport,
    PackSpec, PackStatus, pack_status, parse_pack,
};
pub use registry::{Checksum, Entry, LicenceAcceptance, LicenceRecord, Registry};
pub use resolve::Resolver;
pub use settings::{ApiBases, HostOverride, SourceSettings};
pub use spec::{SourceSpec, SpecError, parse};
