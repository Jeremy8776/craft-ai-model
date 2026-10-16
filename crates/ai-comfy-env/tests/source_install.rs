#![allow(dead_code)]
//! Installing a described file. Every refusal is tested alone with everything else in order,
//! against scripted localhost servers and temporary folders. Nothing is downloaded from a real
//! host and no real ComfyUI is touched.
#[path = "../../ai-openai/tests/support/mod.rs"]
mod support;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use craft_ai_comfy_env::ComfyInstall;
use craft_ai_comfy_env::source::{
    Candidate, Checksum, Consents, Entry, Format, HostOverride, InstallError, Installer, Refusal,
    Registry, SourceKind, SourceSettings,
};
use craft_ai_core::{ErrorKind, SecretString};
use craft_ai_features::Licence;
use sha2::{Digest, Sha256};
use support::{Server, http_bytes};

const TOKEN: &str = "tok-needle-123";
const MARGIN: u64 = 64 * 1024 * 1024;

fn sha(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A body that reads as a safetensors file.
fn weights(extra: usize) -> Vec<u8> {
    let mut b = 2u64.to_le_bytes().to_vec();
    b.extend_from_slice(b"{}");
    b.extend((0..extra).map(|i| (i % 251) as u8));
    b
}

struct Rig {
    dir: tempfile::TempDir,
    installer: Installer,
}

impl Rig {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("Comfy");
        std::fs::create_dir_all(base.join("models/loras")).unwrap();
        let comfy = ComfyInstall::at(&base).unwrap();
        let settings = SourceSettings {
            hosts: Some(HostOverride {
                download: vec!["127.0.0.1".into()],
                token_host: Some("127.0.0.1".into()),
            }),
            allow_loopback: true,
            retry_delay: std::time::Duration::ZERO,
            ..SourceSettings::default()
        };
        let installer = Installer::new(
            comfy,
            settings,
            Registry::at(dir.path().join("model-sources.json")),
            dir.path().join("staging"),
        );
        Self { dir, installer }
    }
    fn models(&self) -> PathBuf {
        self.dir.path().join("Comfy/models/loras")
    }
    fn staging(&self) -> PathBuf {
        self.dir.path().join("staging")
    }
    fn registry(&self) -> Registry {
        Registry::at(self.dir.path().join("model-sources.json"))
    }
    fn install(&self, c: &Candidate, consents: &Consents) -> Result<Entry, InstallError> {
        self.installer.install(
            c,
            "loras",
            consents,
            None,
            &AtomicBool::new(false),
            &mut |_| {},
        )
    }
}

fn origin(s: &Server) -> String {
    s.url.trim_end_matches("/v1").to_owned()
}

fn serving(body: Vec<u8>) -> Server {
    Server::start(move |_| http_bytes(200, &[], &body))
}

/// Like `serving`, but honours `Range: bytes=N-`, so a leftover partial would be resumed.
fn serving_ranges(body: Vec<u8>) -> Server {
    Server::start(
        move |r| match r.header("range").and_then(|v| v.strip_prefix("bytes=")) {
            Some(range) => {
                let from: usize = range.trim_end_matches('-').parse().unwrap();
                let cr = format!("bytes {from}-{}/{}", body.len() - 1, body.len());
                http_bytes(206, &[("Content-Range", cr.as_str())], &body[from..])
            }
            None => http_bytes(200, &[], &body),
        },
    )
}

fn candidate(url: &str, name: &str, body: &[u8], published: bool) -> Candidate {
    Candidate {
        kind: SourceKind::HuggingFace,
        url: Some(format!("{url}/f")),
        pin: "c1".into(),
        folder: String::new(),
        filename: name.into(),
        size_bytes: body.len() as u64,
        sha256: published.then(|| sha(body)),
        format: Format::from_name(name),
        licence: Licence {
            name: "apache-2.0".into(),
            url: None,
            accept_required: false,
            note: None,
        },
        gated: false,
        local_path: None,
    }
}

fn names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .map(|r| {
            r.map(|e| e.unwrap().file_name().into_string().unwrap())
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

fn refused(r: Result<Entry, InstallError>) -> Refusal {
    match r {
        Err(InstallError::Refused(why)) => why,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[test]
fn a_verified_file_lands_in_its_folder_and_is_recorded() {
    let body = weights(4000);
    let server = serving(body.clone());
    let rig = Rig::new();
    let entry = rig
        .install(
            &candidate(&origin(&server), "m.safetensors", &body, true),
            &Consents::default(),
        )
        .unwrap();
    assert_eq!(
        std::fs::read(rig.models().join("m.safetensors")).unwrap(),
        body
    );
    assert_eq!(
        (entry.checksum, entry.sha256.as_str()),
        (Checksum::Published, sha(&body).as_str())
    );
    assert_eq!(rig.registry().list().unwrap(), vec![entry]);
    assert!(
        names(&rig.staging())
            .iter()
            .all(|n| !n.ends_with(".safetensors"))
    );
    let saved = std::fs::read_to_string(rig.dir.path().join("model-sources.json")).unwrap();
    assert!(!saved.contains(TOKEN));
}

#[test]
fn a_file_with_no_published_checksum_needs_consent_and_is_recorded_as_observed() {
    let body = weights(4000);
    let server = serving(body.clone());
    let rig = Rig::new();
    let c = candidate(&origin(&server), "m.safetensors", &body, false);
    assert_eq!(
        refused(rig.install(&c, &Consents::default())),
        Refusal::Unverified
    );
    assert_eq!(server.count(), 0);
    assert!(names(&rig.models()).is_empty());
    let entry = rig
        .install(
            &c,
            &Consents {
                unverified: true,
                ..Consents::default()
            },
        )
        .unwrap();
    assert_eq!(entry.checksum, Checksum::Observed);
    assert_eq!(entry.sha256, sha(&body));
}

#[test]
fn pickle_formats_are_refused_unless_consented() {
    let body = b"\x80\x04some pickle".to_vec();
    let server = serving(body.clone());
    let rig = Rig::new();
    let c = candidate(&origin(&server), "m.ckpt", &body, true);
    assert_eq!(
        refused(rig.install(&c, &Consents::default())),
        Refusal::Pickle
    );
    assert_eq!(server.count(), 0);
    let entry = rig
        .install(
            &c,
            &Consents {
                pickle: true,
                ..Consents::default()
            },
        )
        .unwrap();
    assert!(entry.pickle_consented && entry.format == Format::Pickle);
}

#[test]
fn an_unsupported_name_is_refused_before_any_request() {
    let server = serving(vec![1; 10]);
    let rig = Rig::new();
    let c = candidate(&origin(&server), "m.zip", &[1; 10], true);
    assert_eq!(
        refused(rig.install(&c, &Consents::default())),
        Refusal::Unsupported
    );
    assert_eq!(server.count(), 0);
}

#[test]
fn pickle_bytes_under_a_safe_name_are_deleted_not_installed() {
    let mut body = b"PK\x03\x04".to_vec();
    body.extend_from_slice(&[9; 5000]);
    let server = serving(body.clone());
    let rig = Rig::new();
    let c = candidate(&origin(&server), "m.safetensors", &body, true);
    let why = refused(rig.install(&c, &Consents::default()));
    assert!(
        matches!(
            why,
            Refusal::Mislabelled {
                found: Format::Pickle,
                ..
            }
        ),
        "{why:?}"
    );
    assert!(names(&rig.models()).is_empty());
    assert!(rig.registry().list().unwrap().is_empty());
    let leftovers: Vec<_> = walk(&rig.staging());
    assert!(
        leftovers.iter().all(|p| !p.ends_with("m.safetensors")),
        "{leftovers:?}"
    );
}

fn walk(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(read) = std::fs::read_dir(dir) {
        for e in read.flatten() {
            let p = e.path();
            if p.is_dir() {
                out.extend(walk(&p));
            } else {
                out.push(p.to_string_lossy().into_owned());
            }
        }
    }
    out
}

#[test]
fn a_licence_is_accepted_once_per_version_and_asked_again_when_it_changes() {
    let body = weights(3000);
    let server = serving(body.clone());
    let rig = Rig::new();
    let mut c = candidate(&origin(&server), "a.safetensors", &body, true);
    c.licence = Licence {
        name: "openrail".into(),
        url: Some("https://x.example/l".into()),
        accept_required: true,
        note: None,
    };
    assert_eq!(
        refused(rig.install(&c, &Consents::default())),
        Refusal::Licence
    );
    let first = rig
        .install(
            &c,
            &Consents {
                licence: true,
                ..Consents::default()
            },
        )
        .unwrap();
    assert_eq!(
        first.licence_accepted.as_ref().map(|a| a.name.as_str()),
        Some("openrail")
    );

    // Same model version and terms, another file: remembered.
    let mut same = c.clone();
    same.filename = "b.safetensors".into();
    assert!(rig.install(&same, &Consents::default()).is_ok());
    // A new version, or changed terms: asked again.
    let mut newer = c.clone();
    newer.filename = "c.safetensors".into();
    newer.pin = "c2".into();
    assert_eq!(
        refused(rig.install(&newer, &Consents::default())),
        Refusal::Licence
    );
    let mut changed = c.clone();
    changed.filename = "d.safetensors".into();
    changed.licence.url = Some("https://x.example/other".into());
    assert_eq!(
        refused(rig.install(&changed, &Consents::default())),
        Refusal::Licence
    );
    let mut other_kind = c;
    other_kind.filename = "e.safetensors".into();
    other_kind.kind = SourceKind::Civitai;
    assert_eq!(
        refused(rig.install(&other_kind, &Consents::default())),
        Refusal::Licence
    );
}

#[test]
fn a_file_already_there_is_never_overwritten() {
    let body = weights(3000);
    let server = serving(body.clone());
    let rig = Rig::new();
    std::fs::write(rig.models().join("m.safetensors"), b"mine").unwrap();
    let c = candidate(&origin(&server), "m.safetensors", &body, true);
    assert_eq!(
        refused(rig.install(&c, &Consents::default())),
        Refusal::NameTaken
    );
    assert_eq!(
        std::fs::read(rig.models().join("m.safetensors")).unwrap(),
        b"mine"
    );
    assert_eq!(server.count(), 0);
}

#[test]
fn a_folder_name_cannot_leave_the_models_folder() {
    let body = weights(100);
    let server = serving(body.clone());
    let rig = Rig::new();
    let c = candidate(&origin(&server), "m.safetensors", &body, true);
    for folder in ["", "..", "a/b", "a\\b", "c:", ".hidden"] {
        let r = rig.installer.install(
            &c,
            folder,
            &Consents::default(),
            None,
            &AtomicBool::new(false),
            &mut |_| {},
        );
        assert_eq!(refused(r), Refusal::Folder, "{folder:?}");
    }
    assert_eq!(server.count(), 0);
}

#[test]
fn a_gated_file_needs_a_token_and_the_token_reaches_only_its_host() {
    let body = weights(3000);
    let server = serving(body.clone());
    let rig = Rig::new();
    let mut c = candidate(&origin(&server), "m.safetensors", &body, true);
    c.gated = true;
    assert_eq!(
        refused(rig.install(&c, &Consents::default())),
        Refusal::Token
    );
    let token = SecretString::new(TOKEN);
    rig.installer
        .install(
            &c,
            "loras",
            &Consents::default(),
            Some(&token),
            &AtomicBool::new(false),
            &mut |_| {},
        )
        .unwrap();
    assert_eq!(
        server.recorded()[0].header("authorization"),
        Some(&*format!("Bearer {TOKEN}"))
    );
}

#[test]
fn disk_space_is_checked_with_a_margin_before_downloading() {
    let body = weights(3000);
    let server = serving(body.clone());
    let need = body.len() as u64 + MARGIN;
    let c = candidate(&origin(&server), "m.safetensors", &body, true);
    let rig = Rig::new();
    let installer = |free: Option<u64>| {
        Rig::new()
            .installer
            .with_free_space(Box::new(move |_| free))
    };
    let go = |free: Option<u64>| {
        installer(free).install(
            &c,
            "loras",
            &Consents::default(),
            None,
            &AtomicBool::new(false),
            &mut |_| {},
        )
    };
    assert_eq!(
        refused(go(Some(need - 1))),
        Refusal::NoSpace {
            needed: need,
            available: need - 1
        }
    );
    assert_eq!(server.count(), 0);
    assert!(go(Some(need)).is_ok());
    assert!(go(None).is_ok(), "an unknown amount does not block");
    let _ = rig;
}

#[test]
fn an_absurd_size_is_refused() {
    let rig = Rig::new();
    let mut c = candidate("http://127.0.0.1:1", "m.safetensors", &[1], true);
    c.size_bytes = 512 * 1024 * 1024 * 1024 + 1;
    assert_eq!(
        refused(rig.install(&c, &Consents::default())),
        Refusal::TooLarge
    );
    c.size_bytes = 0;
    assert_eq!(
        refused(rig.install(&c, &Consents::default())),
        Refusal::TooLarge
    );
}

#[test]
fn a_local_file_is_copied_and_the_original_is_left_alone() {
    let rig = Rig::new();
    let body = weights(5000);
    let src = rig.dir.path().join("mine.safetensors");
    std::fs::write(&src, &body).unwrap();
    let c = Candidate {
        kind: SourceKind::Local,
        url: None,
        pin: "local".into(),
        folder: String::new(),
        filename: "mine.safetensors".into(),
        size_bytes: body.len() as u64,
        sha256: None,
        format: Format::Safetensors,
        licence: Licence {
            name: "unknown".into(),
            url: None,
            accept_required: true,
            note: None,
        },
        gated: false,
        local_path: Some(src.clone()),
    };
    assert_eq!(
        refused(rig.install(&c, &Consents::default())),
        Refusal::Licence
    );
    let entry = rig
        .install(
            &c,
            &Consents {
                licence: true,
                ..Consents::default()
            },
        )
        .unwrap();
    assert_eq!(
        (entry.checksum, entry.sha256.as_str()),
        (Checksum::Local, sha(&body).as_str())
    );
    assert_eq!(
        std::fs::read(rig.models().join("mine.safetensors")).unwrap(),
        body
    );
    assert_eq!(std::fs::read(&src).unwrap(), body);
    // Another local file has no version in common with this one: its licence is asked again.
    let mut second = c.clone();
    second.filename = "other.safetensors".into();
    assert_eq!(
        refused(rig.install(&second, &Consents::default())),
        Refusal::Licence
    );

    // A local file that is not what its name says is refused and left alone too.
    let fake = rig.dir.path().join("fake.safetensors");
    std::fs::write(&fake, b"PK\x03\x04zzzzzzzz").unwrap();
    // A file that is not the size it was described as is not copied.
    let mut grown = c.clone();
    grown.filename = "grown.safetensors".into();
    grown.size_bytes += 1;
    assert!(matches!(
        rig.install(
            &grown,
            &Consents {
                licence: true,
                ..Consents::default()
            }
        ),
        Err(InstallError::Failed(_))
    ));
    assert!(!rig.models().join("grown.safetensors").exists());
    let mut bad = c.clone();
    bad.filename = "fake.safetensors".into();
    bad.size_bytes = 12;
    bad.local_path = Some(fake.clone());
    let why = refused(rig.install(
        &bad,
        &Consents {
            licence: true,
            ..Consents::default()
        },
    ));
    assert!(matches!(why, Refusal::Mislabelled { .. }));
    assert!(fake.exists() && !rig.models().join("fake.safetensors").exists());
}

#[test]
fn two_sources_with_the_same_file_name_do_not_share_a_partial_download() {
    let rig = Rig::new();
    let first: Vec<u8> = weights(3 * 1024 * 1024);
    // Different from the first all the way through, so a partial of the first can never be
    // mistaken for the start of the second.
    let second: Vec<u8> = {
        let mut b = 2u64.to_le_bytes().to_vec();
        b.extend_from_slice(b"{}");
        b.extend((0..3 * 1024 * 1024).map(|i| (i % 241) as u8 ^ 0x5a));
        b
    };
    let a = serving_ranges(first.clone());
    let b = serving_ranges(second.clone());
    // Start the first and cancel it once it has some bytes on disk.
    let cancel = AtomicBool::new(false);
    let ca = candidate(&origin(&a), "m.safetensors", &first, false);
    let r = rig.installer.install(
        &ca,
        "loras",
        &Consents {
            unverified: true,
            ..Consents::default()
        },
        None,
        &cancel,
        &mut |_| {
            cancel.store(true, Ordering::Relaxed);
        },
    );
    assert!(
        matches!(&r, Err(InstallError::Failed(e)) if e.kind == ErrorKind::Cancelled),
        "{r:?}"
    );
    assert!(names(&rig.models()).is_empty());
    // The second source, unverified, must come out as its own bytes.
    let cb = candidate(&origin(&b), "m.safetensors", &second, false);
    let entry = rig
        .install(
            &cb,
            &Consents {
                unverified: true,
                ..Consents::default()
            },
        )
        .unwrap();
    assert_eq!(entry.sha256, sha(&second));
    assert_eq!(
        std::fs::read(rig.models().join("m.safetensors")).unwrap(),
        second
    );
}

#[test]
fn the_hosts_in_the_settings_reach_the_downloader() {
    let body = weights(3000);
    let server = serving(body.clone());
    let rig = Rig::new();
    let c = candidate(
        &origin(&server).replace("127.0.0.1", "localhost"),
        "m.safetensors",
        &body,
        true,
    );
    assert!(matches!(
        rig.install(&c, &Consents::default()),
        Err(InstallError::Failed(_))
    ));
    assert_eq!(server.count(), 0);
    assert!(names(&rig.models()).is_empty());
    assert!(rig.registry().list().unwrap().is_empty());
}

#[test]
fn the_registry_file_is_read_with_care() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("model-sources.json");
    let registry = Registry::at(path.clone());
    assert!(registry.list().unwrap().is_empty(), "no file yet");
    std::fs::write(&path, b"{ not json").unwrap();
    assert!(registry.list().is_err());
    std::fs::write(&path, vec![b' '; 5 * 1024 * 1024]).unwrap();
    assert!(registry.list().is_err());
}

#[test]
fn removing_a_record_leaves_the_file_where_it_is() {
    let body = weights(3000);
    let server = serving(body.clone());
    let rig = Rig::new();
    rig.install(
        &candidate(&origin(&server), "m.safetensors", &body, true),
        &Consents::default(),
    )
    .unwrap();
    assert!(rig.registry().remove("loras", "m.safetensors").unwrap());
    assert!(!rig.registry().remove("loras", "m.safetensors").unwrap());
    assert!(rig.registry().list().unwrap().is_empty());
    assert!(rig.models().join("m.safetensors").exists());
    assert!(names(rig.dir.path()).iter().all(|n| !n.contains(".tmp")));
}

#[test]
fn a_leftover_file_in_staging_is_never_trusted() {
    let body = weights(4000);
    let server = serving(body.clone());
    let rig = Rig::new();
    let c = candidate(&origin(&server), "m.safetensors", &body, true);
    // Something of exactly the right size, planted under the final name in this source's staging.
    let stage = rig.installer.staging_dir_for(&c);
    std::fs::create_dir_all(&stage).unwrap();
    std::fs::write(stage.join("m.safetensors"), vec![0u8; body.len()]).unwrap();
    rig.install(&c, &Consents::default()).unwrap();
    assert_eq!(
        std::fs::read(rig.models().join("m.safetensors")).unwrap(),
        body
    );
    assert_eq!(server.count(), 1, "it was fetched, not taken from staging");
}

#[test]
fn disk_space_is_read_for_both_the_staging_and_the_model_folder() {
    let body = weights(3000);
    let server = serving(body.clone());
    let c = candidate(&origin(&server), "m.safetensors", &body, true);
    // The model folder exists (it ends in "loras"); the staging folder does not yet, so its
    // nearest existing parent is asked.
    let go = |staging_free: u64, models_free: u64| {
        Rig::new()
            .installer
            .with_free_space(Box::new(move |p| {
                Some(if p.ends_with("loras") {
                    models_free
                } else {
                    staging_free
                })
            }))
            .check(&c, "loras", &Consents::default(), false)
    };
    assert!(matches!(
        go(u64::MAX, 5),
        Err(Refusal::NoSpace { available: 5, .. })
    ));
    assert!(matches!(
        go(5, u64::MAX),
        Err(Refusal::NoSpace { available: 5, .. })
    ));
    assert!(go(u64::MAX, u64::MAX).is_ok());
}
