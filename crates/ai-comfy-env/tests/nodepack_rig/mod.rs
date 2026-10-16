#![allow(dead_code)]
//! Shared by the node pack tests: a small tar writer for good and hostile archives, scripted
//! localhost servers standing in for GitHub, and a rig that installs into a temporary ComfyUI
//! folder. No real repository is read and no real ComfyUI is touched.
#[path = "../../../ai-openai/tests/support/mod.rs"]
pub mod support;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use craft_ai_comfy_env::ComfyInstall;
use craft_ai_comfy_env::source::{
    ApiBases, HostOverride, Installer, PackConsent, PackError, PackRefusal, Registry, Resolver,
    SourceSettings, parse_pack,
};
use flate2::Compression;
use flate2::write::GzEncoder;
use serde_json::json;
use support::{Reply, Server, http_bytes};

pub const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

// ---- a small tar writer, enough to build good and hostile archives ----

pub fn header(name: &str, typeflag: u8, size: usize, link: &str) -> Vec<u8> {
    let mut h = vec![0u8; 512];
    h[..name.len().min(100)].copy_from_slice(&name.as_bytes()[..name.len().min(100)]);
    h[100..107].copy_from_slice(b"0000644");
    h[108..115].copy_from_slice(b"0000000");
    h[116..123].copy_from_slice(b"0000000");
    h[124..135].copy_from_slice(format!("{size:011o}").as_bytes());
    h[136..147].copy_from_slice(b"00000000000");
    h[156] = typeflag;
    h[157..157 + link.len()].copy_from_slice(link.as_bytes());
    h[257..263].copy_from_slice(b"ustar\0");
    h[263..265].copy_from_slice(b"00");
    h[148..156].copy_from_slice(b"        ");
    let sum: u32 = h.iter().map(|b| u32::from(*b)).sum();
    h[148..155].copy_from_slice(format!("{sum:06o}\0").as_bytes());
    h[155] = b' ';
    h
}

pub fn entry(name: &str, typeflag: u8, data: &[u8], link: &str) -> Vec<u8> {
    let mut out = header(name, typeflag, data.len(), link);
    out.extend_from_slice(data);
    out.resize(out.len().div_ceil(512) * 512, 0);
    out
}

pub fn file(name: &str, data: &[u8]) -> Vec<u8> {
    entry(name, b'0', data, "")
}

pub fn finish(entries: Vec<Vec<u8>>) -> Vec<u8> {
    let mut tar: Vec<u8> = entries.concat();
    tar.extend_from_slice(&[0u8; 1024]);
    tar
}

pub fn gz(tar: &[u8]) -> Vec<u8> {
    let mut e = GzEncoder::new(Vec::new(), Compression::default());
    e.write_all(tar).unwrap();
    e.finish().unwrap()
}

pub fn root() -> String {
    format!("Pack-{COMMIT}")
}

/// A well-formed pack: GitHub's global header, the root folder, and a few files.
pub fn good_archive() -> Vec<u8> {
    let r = root();
    let global = entry(
        "pax_global_header",
        b'g',
        format!("52 comment={COMMIT}\n").as_bytes(),
        "",
    );
    gz(&finish(vec![
        global,
        entry(&format!("{r}/"), b'5', b"", ""),
        file(&format!("{r}/__init__.py"), b"NODE_CLASS_MAPPINGS = {}\n"),
        entry(&format!("{r}/nodes/"), b'5', b"", ""),
        file(&format!("{r}/nodes/a.py"), b"print('a')\n"),
        file(&format!("{r}/requirements.txt"), b"numpy\n"),
        file(&format!("{r}/install.py"), b"import os\n"),
    ]))
}

pub fn archive_of(names: &[&str]) -> Vec<u8> {
    gz(&finish(names.iter().map(|n| file(n, b"x")).collect()))
}

// ---- the rig ----

pub fn origin(s: &Server) -> String {
    s.url.trim_end_matches("/v1").to_owned()
}

pub fn settings(base: &str, hosts: &[&str]) -> SourceSettings {
    SourceSettings {
        bases: ApiBases {
            huggingface: base.into(),
            github_api: base.into(),
            github_web: base.into(),
            civitai: base.into(),
        },
        hosts: Some(HostOverride {
            download: hosts.iter().map(|h| (*h).to_owned()).collect(),
            token_host: None,
        }),
        allow_loopback: true,
        retry_delay: std::time::Duration::ZERO,
        ..SourceSettings::default()
    }
}

pub struct Rig {
    pub dir: tempfile::TempDir,
    pub installer: Installer,
}

impl Rig {
    pub fn new(base: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let base_dir = dir.path().join("Comfy");
        std::fs::create_dir_all(base_dir.join("custom_nodes")).unwrap();
        let installer = Installer::new(
            ComfyInstall::at(&base_dir).unwrap(),
            settings(base, &["127.0.0.1"]),
            Registry::at(dir.path().join("model-sources.json")),
            dir.path().join("staging"),
        );
        Self { dir, installer }
    }
    pub fn nodes(&self) -> PathBuf {
        self.dir.path().join("Comfy/custom_nodes")
    }
    pub fn registry(&self) -> Registry {
        Registry::at(self.dir.path().join("model-sources.json"))
    }
    pub fn candidate(&self, base: &str) -> craft_ai_comfy_env::source::PackCandidate {
        let spec = parse_pack("https://github.com/owner/Pack").unwrap();
        Resolver::new(settings(base, &["127.0.0.1"]))
            .resolve_pack(&spec, None)
            .unwrap()
    }
}

pub fn api_server() -> Server {
    Server::start(|r| {
        if r.path == "/repos/owner/Pack" {
            Reply::Json(
                200,
                json!({"default_branch": "main", "private": false, "archived": false,
                       "license": {"spdx_id": "MIT"}})
                .to_string(),
            )
        } else if r.path.starts_with("/repos/owner/Pack/commits/") {
            Reply::Json(
                200,
                json!({"sha": COMMIT, "commit": {"message": "Add the nodes\n\nA long body.",
                       "committer": {"date": "2026-10-01T12:00:00Z"}}})
                .to_string(),
            )
        } else {
            Reply::Json(404, "{}".into())
        }
    })
}

/// The API and the archive on one server.
pub fn host(archive: Vec<u8>) -> Server {
    Server::start(move |r| {
        if r.path == "/owner/Pack/archive/".to_owned() + COMMIT + ".tar.gz" {
            http_bytes(200, &[], &archive)
        } else if r.path == "/repos/owner/Pack" {
            Reply::Json(
                200,
                json!({"default_branch": "main", "private": false, "archived": false,
                       "license": {"spdx_id": "MIT"}})
                .to_string(),
            )
        } else if r.path.starts_with("/repos/owner/Pack/commits/") {
            Reply::Json(
                200,
                json!({"sha": COMMIT, "commit": {"message": "Add the nodes\n\nA long body.",
                       "committer": {"date": "2026-10-01T12:00:00Z"}}})
                .to_string(),
            )
        } else {
            Reply::Json(404, "{}".into())
        }
    })
}

pub fn consent() -> PackConsent {
    PackConsent {
        commit: COMMIT.to_owned(),
    }
}

pub fn go(
    rig: &Rig,
    c: &craft_ai_comfy_env::source::PackCandidate,
) -> Result<craft_ai_comfy_env::source::PackEntry, PackError> {
    rig.installer
        .install_pack(c, Some(&consent()), &AtomicBool::new(false))
}

pub fn refusal<T: std::fmt::Debug>(r: Result<T, PackError>) -> PackRefusal {
    match r {
        Err(PackError::Refused(why)) => why,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

pub fn listing(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    fn walk(dir: &Path, root: &Path, out: &mut Vec<String>) {
        if let Ok(read) = std::fs::read_dir(dir) {
            for e in read.flatten() {
                let p = e.path();
                out.push(
                    p.strip_prefix(root)
                        .unwrap()
                        .to_string_lossy()
                        .replace('\\', "/"),
                );
                if p.is_dir() {
                    walk(&p, root, out);
                }
            }
        }
    }
    walk(dir, dir, &mut out);
    out.sort();
    out
}
