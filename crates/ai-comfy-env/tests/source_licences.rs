//! The record of licences a person accepted: kept apart from the installed files, asked again when
//! the terms or the version change, never shared between models, and reported (not guessed) when
//! the file is damaged. Temporary folders only; a file on this computer stands in for a download,
//! so no server is needed and nothing is fetched.
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use craft_ai_comfy_env::ComfyInstall;
use craft_ai_comfy_env::source::{
    Candidate, Checksum, Consents, Entry, Format, InstallError, Installer, LicenceAcceptance,
    Refusal, Registry, SourceKind, SourceSettings,
};
use craft_ai_features::Licence;

struct Rig {
    dir: tempfile::TempDir,
    installer: Installer,
}

impl Rig {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("Comfy");
        std::fs::create_dir_all(base.join("models/loras")).unwrap();
        let installer = Installer::new(
            ComfyInstall::at(&base).unwrap(),
            SourceSettings::default(),
            Registry::at(dir.path().join("model-sources.json")),
            dir.path().join("staging"),
        );
        Self { dir, installer }
    }
    fn registry(&self) -> Registry {
        Registry::at(self.dir.path().join("model-sources.json"))
    }
    fn file(&self) -> PathBuf {
        self.dir.path().join("model-sources.json")
    }
    fn check(&self, c: &Candidate) -> Result<PathBuf, Refusal> {
        self.installer
            .check(c, "loras", &Consents::default(), false)
    }
}

fn licence(name: &str, url: Option<&str>, required: bool) -> Licence {
    Licence {
        name: name.into(),
        url: url.map(str::to_owned),
        accept_required: required,
        note: None,
    }
}

/// A hosted model at a pinned version under terms that must be accepted.
fn hosted(pin: &str) -> Candidate {
    Candidate {
        kind: SourceKind::HuggingFace,
        url: Some(format!(
            "https://huggingface.co/o/m/resolve/{pin}/a.safetensors"
        )),
        pin: pin.into(),
        folder: String::new(),
        filename: "a.safetensors".into(),
        size_bytes: 1000,
        sha256: Some("0".repeat(64)),
        format: Format::Safetensors,
        licence: licence("openrail", Some("https://x.example/l"), true),
        gated: false,
        local_path: None,
    }
}

fn local(filename: &str) -> Candidate {
    Candidate {
        kind: SourceKind::Local,
        url: None,
        pin: "local".into(),
        filename: filename.into(),
        local_path: None,
        sha256: None,
        ..hosted("local")
    }
}

#[test]
fn nothing_is_accepted_until_the_person_says_so() {
    let rig = Rig::new();
    let c = hosted("c1");
    assert!(!rig.registry().licence_accepted(&c).unwrap());
    assert_eq!(rig.check(&c), Err(Refusal::Licence));
    assert!(rig.registry().licences().unwrap().is_empty());
}

#[test]
fn an_acceptance_is_remembered_with_when_and_for_what() {
    let rig = Rig::new();
    let c = hosted("c1");
    rig.registry().accept_licence(&c, 1_700_000_000).unwrap();
    assert!(rig.registry().licence_accepted(&c).unwrap());
    assert!(rig.check(&c).is_ok(), "no consent needed a second time");
    let on_record = rig.registry().licences().unwrap();
    assert_eq!(on_record.len(), 1);
    let r = &on_record[0];
    assert_eq!(
        (r.kind, r.pin.as_str(), r.name.as_str(), r.at),
        (SourceKind::HuggingFace, "c1", "openrail", 1_700_000_000)
    );
    assert_eq!(r.url.as_deref(), Some("https://x.example/l"));
}

#[test]
fn accepting_twice_keeps_one_record_and_the_first_time() {
    let rig = Rig::new();
    let c = hosted("c1");
    rig.registry().accept_licence(&c, 10).unwrap();
    rig.registry().accept_licence(&c, 20).unwrap();
    let on_record = rig.registry().licences().unwrap();
    assert_eq!((on_record.len(), on_record[0].at), (1, 10));
}

#[test]
fn terms_that_need_no_acceptance_leave_no_record() {
    let rig = Rig::new();
    let mut c = hosted("c1");
    c.licence = licence("apache-2.0", None, false);
    rig.registry().accept_licence(&c, 10).unwrap();
    assert!(rig.registry().licences().unwrap().is_empty());
}

#[test]
fn a_new_version_changed_terms_or_another_source_ask_again() {
    let rig = Rig::new();
    let c = hosted("c1");
    rig.registry().accept_licence(&c, 10).unwrap();

    let newer = hosted("c2");
    assert_eq!(rig.check(&newer), Err(Refusal::Licence), "new version");

    let mut renamed = c.clone();
    renamed.licence.name = "openrail++".into();
    assert_eq!(rig.check(&renamed), Err(Refusal::Licence), "new name");

    let mut moved = c.clone();
    moved.licence.url = Some("https://x.example/other".into());
    assert_eq!(rig.check(&moved), Err(Refusal::Licence), "new link");

    let mut dropped = c.clone();
    dropped.licence.url = None;
    assert_eq!(rig.check(&dropped), Err(Refusal::Licence), "link removed");

    let mut elsewhere = c;
    elsewhere.kind = SourceKind::Civitai;
    assert_eq!(rig.check(&elsewhere), Err(Refusal::Licence), "another kind");
}

#[test]
fn one_local_file_does_not_inherit_anothers_acceptance() {
    let rig = Rig::new();
    rig.registry()
        .accept_licence(&local("one.safetensors"), 10)
        .unwrap();
    assert!(rig.check(&local("one.safetensors")).is_ok());
    assert_eq!(rig.check(&local("two.safetensors")), Err(Refusal::Licence));
}

#[test]
fn an_acceptance_does_not_depend_on_an_installed_file() {
    let rig = Rig::new();
    let c = hosted("c1");
    rig.registry().accept_licence(&c, 10).unwrap();
    assert!(
        rig.registry().list().unwrap().is_empty(),
        "no file was ever installed"
    );
    assert!(rig.registry().licence_accepted(&c).unwrap());
}

#[test]
fn an_acceptance_can_be_taken_back() {
    let rig = Rig::new();
    let c = hosted("c1");
    rig.registry().accept_licence(&c, 10).unwrap();
    assert!(rig.registry().forget_licence(&c).unwrap());
    assert_eq!(rig.check(&c), Err(Refusal::Licence));
    assert!(
        !rig.registry().forget_licence(&c).unwrap(),
        "nothing left to forget"
    );
}

#[test]
fn installing_with_consent_leaves_a_record_that_outlives_the_file() {
    let rig = Rig::new();
    let src = rig.dir.path().join("mine.safetensors");
    let mut body = 2u64.to_le_bytes().to_vec();
    body.extend_from_slice(b"{}");
    std::fs::write(&src, &body).unwrap();
    let mut c = local("mine.safetensors");
    c.size_bytes = body.len() as u64;
    c.local_path = Some(src);
    let consents = Consents {
        licence: true,
        ..Consents::default()
    };
    let entry = rig
        .installer
        .install(
            &c,
            "loras",
            &consents,
            None,
            &AtomicBool::new(false),
            &mut |_| {},
        )
        .unwrap();
    assert!(
        rig.registry()
            .licences()
            .unwrap()
            .iter()
            .any(|r| r.filename.as_deref() == Some("mine.safetensors"))
    );

    assert!(
        rig.registry()
            .remove(&entry.folder, &entry.filename)
            .unwrap()
    );
    assert!(rig.registry().list().unwrap().is_empty());
    assert!(
        rig.registry().licence_accepted(&c).unwrap(),
        "the person's yes is not the file's"
    );
}

#[test]
fn a_refused_install_records_no_acceptance() {
    let rig = Rig::new();
    let mut c = hosted("c1");
    c.size_bytes = 0; // refused as too large before anything else happens
    let consents = Consents {
        licence: true,
        ..Consents::default()
    };
    let r = rig.installer.install(
        &c,
        "loras",
        &consents,
        None,
        &AtomicBool::new(false),
        &mut |_| {},
    );
    assert!(matches!(r, Err(InstallError::Refused(Refusal::TooLarge))));
    assert!(rig.registry().licences().unwrap().is_empty());
}

#[test]
fn an_acceptance_written_before_the_record_existed_still_counts() {
    let rig = Rig::new();
    let c = hosted("c1");
    let old = Entry {
        kind: c.kind,
        address: c.url.clone().unwrap(),
        pin: c.pin.clone(),
        folder: "loras".into(),
        filename: "earlier.safetensors".into(),
        size_bytes: 10,
        sha256: "0".repeat(64),
        checksum: Checksum::Published,
        format: Format::Safetensors,
        licence: c.licence.clone(),
        licence_accepted: Some(LicenceAcceptance {
            name: c.licence.name.clone(),
            url: c.licence.url.clone(),
            at: 5,
        }),
        pickle_consented: false,
        installed_at: 5,
    };
    rig.registry().record(old).unwrap();
    assert!(rig.registry().licences().unwrap().is_empty());
    assert!(rig.check(&c).is_ok());
    assert!(rig.registry().forget_licence(&c).unwrap());
    assert_eq!(
        rig.check(&c),
        Err(Refusal::Licence),
        "forgetting covers the old form too"
    );
}

#[test]
fn a_file_from_before_the_record_existed_loads() {
    let rig = Rig::new();
    std::fs::write(rig.file(), br#"{"version":1,"entries":[]}"#).unwrap();
    assert!(rig.registry().licences().unwrap().is_empty());
    rig.registry().accept_licence(&hosted("c1"), 7).unwrap();
    assert_eq!(rig.registry().licences().unwrap().len(), 1);
}

#[test]
fn a_damaged_record_is_reported_never_read_as_accepted_or_empty() {
    let rig = Rig::new();
    let c = hosted("c1");
    rig.registry().accept_licence(&c, 10).unwrap();
    std::fs::write(rig.file(), b"{ not json").unwrap();

    assert!(rig.registry().licence_accepted(&c).is_err());
    assert!(rig.registry().licences().is_err());
    assert!(rig.registry().accept_licence(&c, 11).is_err());
    assert!(rig.registry().forget_licence(&c).is_err());
    assert_eq!(
        rig.check(&c),
        Err(Refusal::Record),
        "said before a download starts"
    );
    // Even a person who ticks the box is not taken through a download that cannot be recorded.
    let consents = Consents {
        licence: true,
        ..Consents::default()
    };
    assert_eq!(
        rig.installer.check(&c, "loras", &consents, false),
        Err(Refusal::Record)
    );
    assert_eq!(
        std::fs::read(rig.file()).unwrap(),
        b"{ not json",
        "left as it was"
    );
}
