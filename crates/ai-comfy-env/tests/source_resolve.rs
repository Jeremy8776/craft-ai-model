//! Turning a pasted address into a described, pinned file by asking the host for its size and
//! checksum, against scripted servers that imitate each host's documented replies. Live behaviour
//! of the real hosts is not exercised.
#![allow(dead_code)]
#[path = "../../ai-openai/tests/support/mod.rs"]
mod support;

use craft_ai_comfy_env::source::{
    ApiBases, Candidate, Format, HostOverride, Resolver, SourceKind, SourceSettings, parse,
};
use craft_ai_core::{ErrorKind, SecretString};
use serde_json::{Value, json};
use support::{Reply, Server, http_bytes};

const SHA_A: &str = "aabbccddeeff00112233445566778899aabbccddeeff00112233445566778899";
const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
const TOKEN: &str = "tok-needle-123";
const SIZE: u64 = 4_096;

fn origin(s: &Server) -> String {
    s.url.trim_end_matches("/v1").to_owned()
}

fn settings(base: &str) -> SourceSettings {
    SourceSettings {
        bases: ApiBases {
            huggingface: base.into(),
            github_api: base.into(),
            github_web: base.into(),
            civitai: base.into(),
        },
        hosts: Some(HostOverride {
            download: vec!["127.0.0.1".into()],
            token_host: Some("127.0.0.1".into()),
        }),
        allow_loopback: true,
        ..SourceSettings::default()
    }
}

fn resolve(
    base: &str,
    address: &str,
    token: Option<&str>,
) -> Result<Candidate, craft_ai_core::AiError> {
    let token = token.map(SecretString::new);
    Resolver::new(settings(base)).resolve(&parse(address).unwrap(), token.as_ref())
}

fn json_reply(v: Value) -> Reply {
    Reply::Json(200, v.to_string())
}

fn hf_server(info: Value, paths: Value) -> Server {
    Server::start(move |r| {
        if r.path.contains("/revision/") {
            json_reply(info.clone())
        } else if r.path.contains("/paths-info/") {
            json_reply(paths.clone())
        } else {
            Reply::Json(404, "{}".into())
        }
    })
}

fn hf_info(license: &str, gated: Value) -> Value {
    json!({"sha": COMMIT, "gated": gated, "cardData": {"license": license}})
}

fn hf_entry(size: u64, lfs: Option<Value>) -> Value {
    let mut e =
        json!({"type": "file", "path": "dir/m.safetensors", "size": size, "oid": "1".repeat(40)});
    if let Some(l) = lfs {
        e["lfs"] = l;
    }
    json!([e])
}

const HF_ADDRESS: &str = "https://huggingface.co/org/name/blob/main/dir/m.safetensors";

#[test]
fn a_hugging_face_file_is_pinned_to_a_commit_with_the_published_checksum() {
    let server = hf_server(
        hf_info("apache-2.0", json!(false)),
        hf_entry(
            SIZE,
            Some(json!({"oid": SHA_A.to_uppercase(), "size": SIZE, "pointerSize": 134})),
        ),
    );
    let c = resolve(&origin(&server), HF_ADDRESS, None).unwrap();
    assert_eq!(c.kind, SourceKind::HuggingFace);
    assert_eq!(c.pin, COMMIT);
    assert_eq!((c.filename.as_str(), c.size_bytes), ("m.safetensors", SIZE));
    assert_eq!(c.sha256.as_deref(), Some(SHA_A));
    assert_eq!(c.format, Format::Safetensors);
    assert_eq!(c.licence.name, "apache-2.0");
    let n = c.needs();
    assert!(!n.licence && !n.unverified && !n.pickle && !n.token);
    let url = c.download_url().unwrap();
    assert_eq!(
        url,
        format!(
            "{}/org/name/resolve/{COMMIT}/dir/m.safetensors",
            origin(&server)
        )
    );
    // The revision was asked for by name, then everything else by the pinned commit.
    let seen: Vec<_> = server.recorded().iter().map(|r| r.path.clone()).collect();
    assert!(seen[0].ends_with("/revision/main"), "{seen:?}");
    assert!(
        seen[1].contains(&format!("/paths-info/{COMMIT}")),
        "{seen:?}"
    );
}

#[test]
fn a_plain_git_file_has_no_published_checksum_and_a_gated_repo_needs_a_token() {
    let server = hf_server(
        // Gated with a permissive licence: the gate alone makes it need accepting.
        hf_info("apache-2.0", json!("manual")),
        hf_entry(SIZE, None),
    );
    let c = resolve(&origin(&server), HF_ADDRESS, None).unwrap();
    assert_eq!(c.sha256, None);
    let n = c.needs();
    assert!(n.unverified && n.licence && n.token && c.gated);
    assert!(c.licence.accept_required);
}

#[test]
fn a_checksum_the_host_got_wrong_is_an_error_not_a_downgrade() {
    for lfs in [
        json!({"oid": "not-a-hash", "size": SIZE}),
        json!({"oid": SHA_A, "size": SIZE + 1}),
        json!({"size": SIZE}),
    ] {
        let server = hf_server(hf_info("mit", json!(false)), hf_entry(SIZE, Some(lfs)));
        let e = resolve(&origin(&server), HF_ADDRESS, None).unwrap_err();
        assert_eq!(e.kind, ErrorKind::Provider);
    }
}

#[test]
fn replies_that_do_not_describe_the_asked_file_are_refused() {
    let good = hf_entry(SIZE, Some(json!({"oid": SHA_A, "size": SIZE})));
    let cases = [
        (json!({"sha": "main"}), good.clone()),
        (json!({"sha": COMMIT.to_uppercase()}), good.clone()),
        (hf_info("mit", json!(false)), json!([])),
        (
            hf_info("mit", json!(false)),
            json!([{"type": "directory", "path": "dir/m.safetensors", "size": SIZE}]),
        ),
        (
            hf_info("mit", json!(false)),
            json!([{"type": "file", "path": "other.safetensors", "size": SIZE}]),
        ),
        (hf_info("mit", json!(false)), json!({"not": "a list"})),
        (
            hf_info("mit", json!(false)),
            json!([{"type": "file", "path": "dir/m.safetensors", "size": 0}]),
        ),
    ];
    for (info, paths) in cases {
        let server = hf_server(info, paths);
        assert!(resolve(&origin(&server), HF_ADDRESS, None).is_err());
    }
}

#[test]
fn an_unsupported_file_type_is_refused_when_resolving() {
    let server = hf_server(hf_info("mit", json!(false)), hf_entry(SIZE, None));
    let e = resolve(
        &origin(&server),
        "https://huggingface.co/org/name/blob/main/dir/m.zip",
        None,
    )
    .unwrap_err();
    assert_eq!(e.kind, ErrorKind::InvalidRequest);
    assert_eq!(server.count(), 0, "refused before asking the host");
}

#[test]
fn a_missing_or_private_repo_is_told_apart() {
    let gone = Server::start(|_| Reply::Json(404, "{}".into()));
    assert_eq!(
        resolve(&origin(&gone), HF_ADDRESS, None).unwrap_err().kind,
        ErrorKind::InvalidRequest
    );
    let private = Server::start(|_| Reply::Json(401, "{}".into()));
    let e = resolve(&origin(&private), HF_ADDRESS, None).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Auth);
    assert!(e.message.contains("token"), "{}", e.message);
}

#[test]
fn the_token_goes_only_to_the_host_its_kind_names() {
    let server = hf_server(
        hf_info("mit", json!(false)),
        hf_entry(SIZE, Some(json!({"oid": SHA_A, "size": SIZE}))),
    );
    resolve(&origin(&server), HF_ADDRESS, Some(TOKEN)).unwrap();
    for r in server.recorded() {
        assert_eq!(r.header("authorization"), Some(&*format!("Bearer {TOKEN}")));
    }
    // Control: the same call with the token reserved for another host sends none.
    let other = hf_server(
        hf_info("mit", json!(false)),
        hf_entry(SIZE, Some(json!({"oid": SHA_A, "size": SIZE}))),
    );
    let mut s = settings(&origin(&other));
    s.hosts = Some(HostOverride {
        download: vec!["127.0.0.1".into()],
        token_host: Some("huggingface.co".into()),
    });
    let token = SecretString::new(TOKEN);
    Resolver::new(s)
        .resolve(&parse(HF_ADDRESS).unwrap(), Some(&token))
        .unwrap();
    assert!(
        other
            .recorded()
            .iter()
            .all(|r| r.header("authorization").is_none())
    );
}

#[test]
fn a_redirect_from_the_host_is_not_followed() {
    let elsewhere = Server::start(|_| json_reply(hf_info("mit", json!(false))));
    let target = format!("{}/x", origin(&elsewhere).replace("127.0.0.1", "localhost"));
    let server = Server::start(move |_| {
        Reply::JsonWith(302, vec![("Location", target.clone())], String::new())
    });
    let token = SecretString::new(TOKEN);
    let _ = Resolver::new(settings(&origin(&server)))
        .resolve(&parse(HF_ADDRESS).unwrap(), Some(&token));
    assert_eq!(elsewhere.count(), 0);
}

#[test]
fn an_enormous_reply_is_refused() {
    // Valid JSON followed by padding: cut at the limit it would still parse, so only the size
    // check can refuse it.
    let padded = format!(
        "{}{}",
        hf_info("mit", json!(false)),
        " ".repeat(3 * 1024 * 1024)
    );
    // Everything after the first reply is in order, so only the size check can refuse it.
    let rest = hf_entry(SIZE, Some(json!({"oid": SHA_A, "size": SIZE})));
    let server = Server::start(move |r| {
        if r.path.contains("/revision/") {
            Reply::Json(200, padded.clone())
        } else {
            json_reply(rest.clone())
        }
    });
    assert!(resolve(&origin(&server), HF_ADDRESS, None).is_err());
}

const GH_ADDRESS: &str = "https://github.com/owner/repo/releases/download/v1.2/model.gguf";

fn gh_server(asset: Value, licence: Reply) -> Server {
    Server::start(move |r| {
        if r.path == "/repos/owner/repo/releases/tags/v1.2" {
            json_reply(json!({"tag_name": "v1.2", "assets": [asset.clone()]}))
        } else if r.path == "/repos/owner/repo" {
            match &licence {
                Reply::Json(s, b) => Reply::Json(*s, b.clone()),
                _ => Reply::Json(500, "{}".into()),
            }
        } else {
            Reply::Json(404, "{}".into())
        }
    })
}

fn gh_asset(digest: Value) -> Value {
    json!({"name": "model.gguf", "size": SIZE, "state": "uploaded", "digest": digest,
           "browser_download_url": "http://evil.example/model.gguf"})
}

#[test]
fn a_release_asset_uses_its_published_digest_and_our_own_download_address() {
    let server = gh_server(
        gh_asset(json!(format!("sha256:{SHA_A}"))),
        Reply::Json(200, json!({"license": {"spdx_id": "MIT"}}).to_string()),
    );
    let c = resolve(&origin(&server), GH_ADDRESS, None).unwrap();
    assert_eq!(c.kind, SourceKind::GitHubRelease);
    assert_eq!((c.size_bytes, c.sha256.as_deref()), (SIZE, Some(SHA_A)));
    assert_eq!(c.pin, "v1.2");
    assert_eq!(c.format, Format::Gguf);
    assert!(!c.licence.accept_required, "MIT is plainly permissive");
    assert_eq!(
        c.download_url().unwrap(),
        format!(
            "{}/owner/repo/releases/download/v1.2/model.gguf",
            origin(&server)
        )
    );
}

#[test]
fn a_release_asset_without_a_usable_digest_is_unverified() {
    for digest in [
        json!(null),
        json!("sha1:abcdef"),
        json!("sha256:short"),
        json!(format!("sha512:{SHA_A}")),
        json!(5),
    ] {
        let server = gh_server(gh_asset(digest.clone()), Reply::Json(500, "{}".into()));
        let c = resolve(&origin(&server), GH_ADDRESS, None).unwrap();
        assert_eq!(c.sha256, None, "{digest}");
        assert!(c.needs().unverified);
        assert!(
            c.licence.accept_required,
            "an unknown licence has to be accepted"
        );
    }
}

#[test]
fn a_release_asset_that_is_missing_or_unfinished_is_refused() {
    let mut open = gh_asset(json!(null));
    open["state"] = json!("open");
    let mut wrong = gh_asset(json!(null));
    wrong["name"] = json!("other.gguf");
    for asset in [open, wrong] {
        let server = gh_server(asset, Reply::Json(500, "{}".into()));
        assert!(resolve(&origin(&server), GH_ADDRESS, None).is_err());
    }
}

fn civitai_version(file: Value) -> Value {
    json!({"id": 456, "modelId": 123, "model": {"type": "LORA"}, "files": [file]})
}

fn civ_file(name: &str, base: &str) -> Value {
    json!({"name": name, "type": "Model", "primary": true, "sizeKB": SIZE as f64 / 1024.0,
           "hashes": {"SHA256": SHA_A.to_uppercase()},
           "metadata": {"format": "SafeTensor"},
           "downloadUrl": format!("{base}/api/download/models/456")})
}

/// Imitates the version API and the download endpoint (which answers a one-byte range).
fn civitai_server(make: impl Fn(&str) -> Value + Send + Sync + 'static) -> Server {
    Server::start(move |r| {
        let host = r.header("host").unwrap_or("127.0.0.1").to_owned();
        if r.path == "/api/v1/model-versions/456" {
            json_reply(make(&format!("http://{host}")))
        } else if r.path == "/api/download/models/456" || r.path == "/elsewhere/456" {
            let cr = format!("bytes 0-0/{SIZE}");
            http_bytes(206, &[("Content-Range", cr.as_str())], &[0])
        } else {
            Reply::Json(404, "{}".into())
        }
    })
}

const CIV_ADDRESS: &str = "https://civitai.com/api/download/models/456";

#[test]
fn a_civitai_version_gets_its_exact_size_from_the_download_and_its_folder_from_its_type() {
    let server = civitai_server(|b| civitai_version(civ_file("style.safetensors", b)));
    let c = resolve(&origin(&server), CIV_ADDRESS, None).unwrap();
    assert_eq!(c.kind, SourceKind::Civitai);
    assert_eq!((c.size_bytes, c.sha256.as_deref()), (SIZE, Some(SHA_A)));
    assert_eq!(c.folder, "loras");
    assert_eq!(c.pin, "456");
    assert!(c.licence.accept_required);
    assert_eq!(
        c.licence.url.as_deref(),
        Some(format!("{}/models/123", origin(&server)).as_str())
    );
}

#[test]
fn a_civitai_pickle_file_is_flagged_and_odd_replies_are_refused() {
    let server = civitai_server(|b| civitai_version(civ_file("old.ckpt", b)));
    assert!(
        resolve(&origin(&server), CIV_ADDRESS, None)
            .unwrap()
            .needs()
            .pickle
    );

    let bad: [fn(&str) -> Value; 5] = [
        |b| {
            let mut f = civ_file("m.safetensors", b);
            f["downloadUrl"] = json!(format!("{b}/elsewhere/456"));
            civitai_version(f)
        },
        |b| civitai_version(civ_file("a/b.safetensors", b)),
        |_| civitai_version(civ_file("m.safetensors", "http://evil.example")),
        |b| {
            let mut f = civ_file("m.safetensors", b);
            f["sizeKB"] = json!(999.0);
            civitai_version(f)
        },
        |b| {
            let mut f = civ_file("m.safetensors", b);
            f["hashes"] = json!({"SHA256": "zz"});
            civitai_version(f)
        },
    ];
    for make in bad {
        let server = civitai_server(make);
        assert!(resolve(&origin(&server), CIV_ADDRESS, None).is_err());
    }
}

#[test]
fn a_file_on_this_computer_is_described_without_being_hashed_or_trusted() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mine.safetensors");
    std::fs::write(&path, vec![7u8; 100]).unwrap();
    let c = Resolver::new(SourceSettings::default())
        .resolve(&parse(path.to_str().unwrap()).unwrap(), None)
        .unwrap();
    assert_eq!(c.kind, SourceKind::Local);
    assert_eq!((c.size_bytes, c.sha256.clone()), (100, None));
    assert_eq!(c.local_path.as_deref(), Some(path.as_path()));
    assert!(c.licence.accept_required);
    assert!(!c.needs().unverified, "the person chose their own file");

    // A directory with a believable name is still not a file.
    std::fs::create_dir(dir.path().join("weights.safetensors")).unwrap();
    for bad in [
        dir.path().join("missing.safetensors"),
        dir.path().join("weights.safetensors"),
        dir.path().join("x.zip"),
    ] {
        std::fs::write(dir.path().join("x.zip"), b"z").unwrap();
        let spec = parse(bad.to_str().unwrap()).unwrap();
        assert!(
            Resolver::new(SourceSettings::default())
                .resolve(&spec, None)
                .is_err(),
            "{bad:?}"
        );
    }
}

#[test]
fn listing_requests_obey_the_hosts_of_their_kind() {
    let server = hf_server(hf_info("mit", json!(false)), hf_entry(SIZE, None));
    let mut s = settings(&origin(&server));
    s.hosts = Some(HostOverride {
        download: vec!["huggingface.co".into()],
        token_host: None,
    });
    let e = Resolver::new(s)
        .resolve(&parse(HF_ADDRESS).unwrap(), None)
        .unwrap_err();
    assert_eq!(e.kind, ErrorKind::Provider);
    assert_eq!(server.count(), 0, "the host was not asked");
}
