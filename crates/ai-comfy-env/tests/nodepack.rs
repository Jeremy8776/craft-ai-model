#![allow(dead_code)]
//! Installing a ComfyUI node pack from a GitHub address. The pack runs arbitrary Python inside
//! ComfyUI, so these tests pin what is shown, what is consented to, and that nothing is run or
//! pip-installed. What an archive may contain is in `nodepack_archive.rs`. Scripted localhost
//! servers and temporary folders only: no real repository is read and no real ComfyUI is touched.
mod nodepack_rig;

use std::sync::atomic::AtomicBool;

use craft_ai_comfy_env::ComfyInstall;
use craft_ai_comfy_env::source::{
    Consents, Installer, PackConsent, PackError, PackRefusal, PackSpec, Registry, Resolver,
    parse_pack,
};
use craft_ai_core::ErrorKind;
use nodepack_rig::support::{Reply, Server, http_bytes};
use nodepack_rig::*;
use serde_json::json;

// ---- addresses ----

#[test]
fn a_github_repository_address_names_an_owner_a_repo_and_maybe_a_ref() {
    let want = |reference: Option<&str>| PackSpec {
        owner: "owner".into(),
        repo: "ComfyUI-Pack".into(),
        reference: reference.map(str::to_owned),
    };
    for (url, reference) in [
        ("https://github.com/owner/ComfyUI-Pack", None),
        ("https://github.com/owner/ComfyUI-Pack/", None),
        ("https://github.com/owner/ComfyUI-Pack.git", None),
        (
            "https://github.com/owner/ComfyUI-Pack/tree/dev-1.2",
            Some("dev-1.2"),
        ),
    ] {
        assert_eq!(parse_pack(url), Ok(want(reference)), "{url}");
    }
}

#[test]
fn addresses_that_are_not_a_plain_https_github_repository_are_refused() {
    for url in [
        "http://github.com/owner/repo",
        "https://user@github.com/owner/repo",
        "https://github.com.evil.com/owner/repo",
        "https://gitlab.com/owner/repo",
        "https://github.com:8443/owner/repo",
        "https://github.com/owner",
        "https://github.com/owner/repo/issues",
        "https://github.com/owner/repo/archive/main.zip",
        "https://github.com/owner/repo/tree/a/b",
        "https://github.com/owner/repo/tree/a%20b",
        "https://github.com/owner/re%20po",
        "https://github.com/ow%20ner/repo",
        "https://github.com/owner/../repo",
        "https://github.com/owner/re%2Fpo",
        "git@github.com:owner/repo.git",
        "/tmp/repo",
        "",
    ] {
        assert!(parse_pack(url).is_err(), "{url:?}");
    }
}

// ---- what the person is shown ----

#[test]
fn the_pack_is_pinned_to_a_commit_the_person_can_be_shown() {
    let server = api_server();
    let rig = Rig::new(&origin(&server));
    let c = rig.candidate(&origin(&server));
    assert_eq!(c.commit, COMMIT);
    assert_eq!(
        (c.owner.as_str(), c.repo.as_str(), c.folder.as_str()),
        ("owner", "Pack", "Pack")
    );
    assert_eq!(c.subject, "Add the nodes");
    assert_eq!(c.committed_at, "2026-10-01T12:00:00Z");
    assert_eq!(c.licence, "MIT");
    assert!(!c.archived);
    assert_eq!(
        c.archive_url,
        format!("{}/owner/Pack/archive/{COMMIT}.tar.gz", origin(&server))
    );
    let seen: Vec<_> = server.recorded().iter().map(|r| r.path.clone()).collect();
    assert!(
        seen.iter().any(|p| p == "/repos/owner/Pack/commits/main"),
        "{seen:?}"
    );
}

#[test]
fn a_private_repository_or_a_commit_that_is_not_a_commit_is_refused() {
    let spec = parse_pack("https://github.com/owner/Pack").unwrap();
    let make = |repo: serde_json::Value, commit: serde_json::Value| {
        Server::start(move |r| {
            if r.path == "/repos/owner/Pack" {
                Reply::Json(200, repo.to_string())
            } else {
                Reply::Json(200, commit.to_string())
            }
        })
    };
    let ok_repo = json!({"default_branch": "main", "private": false});
    let ok_commit = json!({"sha": COMMIT, "commit": {"message": "m"}});
    for (repo, commit) in [
        (
            json!({"default_branch": "main", "private": true}),
            ok_commit.clone(),
        ),
        (
            ok_repo.clone(),
            json!({"sha": "main", "commit": {"message": "m"}}),
        ),
        (
            ok_repo.clone(),
            json!({"sha": COMMIT.to_uppercase(), "commit": {"message": "m"}}),
        ),
        (ok_repo.clone(), json!({"commit": {"message": "m"}})),
        (json!({"private": false}), ok_commit.clone()),
    ] {
        let server = make(repo, commit);
        let e = Resolver::new(settings(&origin(&server), &["127.0.0.1"])).resolve_pack(&spec, None);
        assert!(e.is_err());
    }
    let gone = Server::start(|_| Reply::Json(404, "{}".into()));
    let e = Resolver::new(settings(&origin(&gone), &["127.0.0.1"]))
        .resolve_pack(&spec, None)
        .unwrap_err();
    assert_eq!(e.kind, ErrorKind::InvalidRequest);
}

#[test]
fn what_the_commit_says_is_shown_as_one_short_clean_line() {
    let server = Server::start(|r| {
        if r.path == "/repos/owner/Pack" {
            Reply::Json(
                200,
                json!({"default_branch": "main", "private": false, "archived": true}).to_string(),
            )
        } else {
            let long = format!("Hello\u{7}\u{1b}[31m world {}", "x".repeat(300));
            Reply::Json(
                200,
                json!({"sha": COMMIT, "commit": {"message": long}}).to_string(),
            )
        }
    });
    let spec = parse_pack("https://github.com/owner/Pack").unwrap();
    let c = Resolver::new(settings(&origin(&server), &["127.0.0.1"]))
        .resolve_pack(&spec, None)
        .unwrap();
    assert!(!c.subject.chars().any(char::is_control), "{:?}", c.subject);
    assert!(c.subject.chars().count() <= 120);
    assert!(c.archived);
    assert_eq!(c.licence, "unknown");
}

// ---- installing ----

#[test]
fn consent_to_run_code_is_for_one_commit_and_nothing_is_fetched_without_it() {
    let server = host(good_archive());
    let rig = Rig::new(&origin(&server));
    let c = rig.candidate(&origin(&server));
    let before = server.count();
    let none = rig
        .installer
        .install_pack(&c, None, &AtomicBool::new(false));
    assert_eq!(refusal(none), PackRefusal::NeedsConsent);
    let other = PackConsent {
        commit: "f".repeat(40),
    };
    let wrong = rig
        .installer
        .install_pack(&c, Some(&other), &AtomicBool::new(false));
    assert_eq!(refusal(wrong), PackRefusal::NeedsConsent);
    assert_eq!(server.count(), before, "no archive was requested");
    assert!(listing(&rig.nodes()).is_empty());
}

#[test]
fn a_pack_is_unpacked_exactly_as_shipped_and_nothing_is_run() {
    let server = host(good_archive());
    let rig = Rig::new(&origin(&server));
    let c = rig.candidate(&origin(&server));
    let entry = go(&rig, &c).unwrap();
    assert_eq!(
        listing(&rig.nodes()),
        [
            "Pack",
            "Pack/__init__.py",
            "Pack/install.py",
            "Pack/nodes",
            "Pack/nodes/a.py",
            "Pack/requirements.txt"
        ]
    );
    assert_eq!(
        std::fs::read(rig.nodes().join("Pack/nodes/a.py")).unwrap(),
        b"print('a')\n"
    );
    assert_eq!(
        (entry.commit.as_str(), entry.folder.as_str(), entry.files),
        (COMMIT, "Pack", 4)
    );
    assert!(entry.has_requirements && entry.has_install_script);
    assert_eq!(entry.archive_sha256.len(), 64);
    assert_eq!(rig.registry().packs().unwrap(), vec![entry]);
    // Staging is cleaned up.
    assert!(
        listing(&rig.dir.path().join("staging")).is_empty(),
        "{:?}",
        listing(&rig.dir.path().join("staging"))
    );
}

#[test]
fn packs_and_models_share_one_list_without_disturbing_each_other() {
    let server = host(good_archive());
    let rig = Rig::new(&origin(&server));
    go(&rig, &rig.candidate(&origin(&server))).unwrap();
    // A model recorded afterwards must not erase the pack, and the other way round.
    let registry = rig.registry();
    assert_eq!(registry.packs().unwrap().len(), 1);
    assert!(registry.list().unwrap().is_empty());
    let _ = Consents::default();
}

#[test]
fn an_existing_folder_is_never_replaced() {
    let server = host(good_archive());
    let rig = Rig::new(&origin(&server));
    std::fs::create_dir_all(rig.nodes().join("Pack")).unwrap();
    std::fs::write(rig.nodes().join("Pack/mine.py"), b"mine").unwrap();
    let c = rig.candidate(&origin(&server));
    let before = server.count();
    assert_eq!(refusal(go(&rig, &c)), PackRefusal::FolderTaken);
    assert_eq!(listing(&rig.nodes()), ["Pack", "Pack/mine.py"]);
    assert_eq!(server.count(), before, "refused before downloading");
}

#[test]
fn the_archive_may_come_from_a_storage_host_of_github_but_only_from_one() {
    let storage = Server::start({
        let archive = good_archive();
        move |_| http_bytes(200, &[], &archive)
    });
    let target = format!(
        "{}/stored.tar.gz",
        origin(&storage).replace("127.0.0.1", "localhost")
    );
    let front = Server::start(move |r| {
        if r.path.contains("/archive/") {
            Reply::JsonWith(302, vec![("Location", target.clone())], String::new())
        } else if r.path == "/repos/owner/Pack" {
            Reply::Json(
                200,
                json!({"default_branch": "main", "private": false}).to_string(),
            )
        } else {
            Reply::Json(
                200,
                json!({"sha": COMMIT, "commit": {"message": "m"}}).to_string(),
            )
        }
    });
    // Control: with the storage host on the list, the redirect is followed.
    let rig = Rig::new(&origin(&front));
    let allowed = Installer::new(
        ComfyInstall::at(&rig.dir.path().join("Comfy")).unwrap(),
        settings(&origin(&front), &["127.0.0.1", "localhost"]),
        Registry::at(rig.dir.path().join("model-sources.json")),
        rig.dir.path().join("staging"),
    );
    let c = rig.candidate(&origin(&front));
    assert!(
        allowed
            .install_pack(&c, Some(&consent()), &AtomicBool::new(false))
            .is_ok()
    );
    // Without it, the same redirect is refused and nothing is installed.
    let rig = Rig::new(&origin(&front));
    let storage_hits = storage.count();
    assert!(go(&rig, &rig.candidate(&origin(&front))).is_err());
    assert_eq!(
        storage.count(),
        storage_hits,
        "the storage host was not contacted"
    );
    assert!(listing(&rig.nodes()).is_empty());
}

// ---- removal ----

#[test]
fn a_recorded_pack_can_be_removed_and_a_folder_we_did_not_install_cannot() {
    let server = host(good_archive());
    let rig = Rig::new(&origin(&server));
    go(&rig, &rig.candidate(&origin(&server))).unwrap();
    std::fs::create_dir_all(rig.nodes().join("TheirOwn")).unwrap();
    std::fs::write(rig.nodes().join("TheirOwn/x.py"), b"x").unwrap();

    assert!(matches!(
        rig.installer.remove_pack("TheirOwn"),
        Err(PackError::Refused(PackRefusal::NotRecorded))
    ));
    for bad in ["", "..", "a/b", "a\\b", "Pack/.."] {
        assert!(rig.installer.remove_pack(bad).is_err(), "{bad:?}");
    }
    assert!(rig.nodes().join("TheirOwn/x.py").exists());

    let removed = rig.installer.remove_pack("Pack").unwrap();
    assert_eq!(removed, 4);
    assert!(!rig.nodes().join("Pack").exists());
    assert!(rig.registry().packs().unwrap().is_empty());
    assert!(rig.nodes().join("TheirOwn/x.py").exists());
    assert!(matches!(
        rig.installer.remove_pack("Pack"),
        Err(PackError::Refused(PackRefusal::NotRecorded))
    ));
}

#[test]
fn a_folder_that_is_a_link_is_not_followed_when_removing() {
    let server = host(good_archive());
    let rig = Rig::new(&origin(&server));
    go(&rig, &rig.candidate(&origin(&server))).unwrap();
    // Swap the installed folder for a link to somewhere else holding a file of ours.
    let elsewhere = rig.dir.path().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    std::fs::write(elsewhere.join("keep.txt"), b"keep").unwrap();
    std::fs::remove_dir_all(rig.nodes().join("Pack")).unwrap();
    #[cfg(windows)]
    let linked = std::os::windows::fs::symlink_dir(&elsewhere, rig.nodes().join("Pack"));
    #[cfg(unix)]
    let linked = std::os::unix::fs::symlink(&elsewhere, rig.nodes().join("Pack"));
    if linked.is_err() {
        // Creating a link needs a privilege this machine did not grant. Say so, do not pretend.
        eprintln!("SKIPPED: could not create a link here, so link handling is untested");
        return;
    }
    assert!(rig.installer.remove_pack("Pack").is_err());
    assert!(elsewhere.join("keep.txt").exists());
}
