#![allow(dead_code)]
//! What an archive from GitHub may contain. The pack is hostile until it has been read, so
//! every refusal here is checked for the reason it gives, not only for being a refusal: a case
//! that some other check happens to catch would otherwise hide a check that has gone missing.
mod nodepack_rig;

use craft_ai_comfy_env::source::{PackLimits, PackRefusal};
use nodepack_rig::*;

const PLAIN: &str = "a path is not a plain relative path";

#[test]
fn hostile_archives_are_refused_for_the_right_reason_and_leave_nothing_behind() {
    let r = root();
    let cases: Vec<(&str, Vec<u8>, &str)> = vec![
        (
            "parent folder",
            archive_of(&[&format!("{r}/../evil.py")]),
            PLAIN,
        ),
        (
            "parent folder deep",
            archive_of(&[&format!("{r}/a/../../evil.py")]),
            PLAIN,
        ),
        ("absolute", archive_of(&["/etc/evil.py"]), PLAIN),
        (
            "backslash",
            archive_of(&[&format!("{r}/a\\..\\evil.py")]),
            PLAIN,
        ),
        (
            "drive letter",
            archive_of(&[&format!("{r}/C:evil.py")]),
            PLAIN,
        ),
        ("device name", archive_of(&[&format!("{r}/nul.py")]), PLAIN),
        ("trailing dot", archive_of(&[&format!("{r}/evil.")]), PLAIN),
        (
            "pax size",
            gz(&finish(vec![
                entry("pax", b'x', b"13 size=9999\n", ""),
                file(&format!("{r}/a.py"), b"x"),
            ])),
            "an entry uses a header not accepted",
        ),
        (
            "outside the root",
            archive_of(&[&format!("{r}/a.py"), "Other/b.py"]),
            "the archive has more than one top folder",
        ),
        (
            // Windows would stop the second name by itself, so the reason is what proves the check.
            "duplicate by case",
            archive_of(&[&format!("{r}/A.py"), &format!("{r}/a.py")]),
            "two names differ only in case",
        ),
        (
            // A case-sensitive disk would make two folders, Windows and default macOS one.
            "folder by case",
            archive_of(&[&format!("{r}/Dir/a.py"), &format!("{r}/dir/b.py")]),
            "two names differ only in case",
        ),
        (
            "folder by case, deeper down",
            archive_of(&[&format!("{r}/x/Dir/a.py"), &format!("{r}/x/dir/b.py")]),
            "two names differ only in case",
        ),
        (
            "folder entry by case",
            gz(&finish(vec![
                file(&format!("{r}/Dir/a.py"), b"x"),
                entry(&format!("{r}/dir/"), b'5', b"", ""),
            ])),
            "two names differ only in case",
        ),
        (
            "file then folder by case",
            archive_of(&[&format!("{r}/A"), &format!("{r}/a/b.py")]),
            "two names differ only in case",
        ),
        (
            "folder then file by case",
            archive_of(&[&format!("{r}/a/b.py"), &format!("{r}/A")]),
            "two names differ only in case",
        ),
        ("empty", gz(&finish(vec![])), "the archive holds no files"),
        (
            "not gzip",
            b"definitely not an archive".to_vec(),
            "the archive is damaged",
        ),
        (
            "hard link",
            gz(&finish(vec![
                file(&format!("{r}/a.py"), b"x"),
                entry(&format!("{r}/b.py"), b'1', b"", &format!("{r}/a.py")),
            ])),
            "the archive contains links",
        ),
        (
            "symbolic link",
            gz(&finish(vec![entry(
                &format!("{r}/b.py"),
                b'2',
                b"",
                "/etc/passwd",
            )])),
            "the archive contains links",
        ),
        (
            "device file",
            gz(&finish(vec![entry(&format!("{r}/dev"), b'3', b"", "")])),
            "the archive contains special files",
        ),
        (
            "damaged header",
            {
                let mut t = finish(vec![file(&format!("{r}/a.py"), b"x")]);
                t[10] ^= 0xff;
                gz(&t)
            },
            "the archive is damaged",
        ),
        (
            "truncated",
            {
                let t = finish(vec![file(&format!("{r}/a.py"), &[1u8; 2000])]);
                gz(&t[..700])
            },
            "the archive is cut short",
        ),
    ];
    for (why, archive, reason) in cases {
        let server = host(archive);
        let rig = Rig::new(&origin(&server));
        let c = rig.candidate(&origin(&server));
        let result = go(&rig, &c);
        assert_eq!(refusal(result), PackRefusal::Unsafe(reason), "{why}");
        assert!(
            listing(&rig.nodes()).is_empty(),
            "{why}: {:?}",
            listing(&rig.nodes())
        );
        assert!(rig.registry().packs().unwrap().is_empty(), "{why}");
        assert!(
            listing(&rig.dir.path().join("staging")).is_empty(),
            "{why}: staging not cleaned"
        );
        let escaped = rig.dir.path().join("Comfy").join("evil.py");
        assert!(
            !escaped.exists() && !rig.dir.path().join("evil.py").exists(),
            "{why}"
        );
    }
}

#[test]
fn a_long_path_given_by_a_pax_header_is_read_and_checked_like_any_other() {
    let long = format!("{}/{}.py", root(), "n".repeat(150));
    let record = |path: &str| {
        let body = format!(" path={path}\n");
        let mut len = body.len() + 1;
        while (len.to_string().len() + body.len()) != len {
            len = len.to_string().len() + body.len();
        }
        format!("{len}{body}")
    };
    let ok = gz(&finish(vec![
        entry("pax", b'x', record(&long).as_bytes(), ""),
        file("short", b"x"),
    ]));
    let server = host(ok);
    let rig = Rig::new(&origin(&server));
    assert!(go(&rig, &rig.candidate(&origin(&server))).is_ok());

    let evil = format!("{}/../../evil.py", root());
    let bad = gz(&finish(vec![
        entry("pax", b'x', record(&evil).as_bytes(), ""),
        file("short", b"x"),
    ]));
    let server = host(bad);
    let rig = Rig::new(&origin(&server));
    assert!(go(&rig, &rig.candidate(&origin(&server))).is_err());
    assert!(listing(&rig.nodes()).is_empty());
}

#[test]
fn size_limits_apply_to_the_download_the_unpacked_total_and_the_entry_count() {
    let r = root();
    let big = gz(&finish(vec![file(
        &format!("{r}/big.bin"),
        &vec![0u8; 3 * 1024 * 1024],
    )]));
    let many: Vec<Vec<u8>> = (0..30)
        .map(|i| file(&format!("{r}/f{i}.py"), b"x"))
        .collect();
    let many = gz(&finish(many));
    let small = PackLimits {
        max_download: 100 * 1024,
        max_expanded: 1024 * 1024,
        max_entries: 20,
        ..PackLimits::default()
    };
    // Compresses well, so the download passes while the unpacked size is the problem.
    let server = host(big);
    let mut rig = Rig::new(&origin(&server));
    let c = rig.candidate(&origin(&server));
    rig.installer.set_pack_limits(small.clone());
    let r1 = go(&rig, &c);
    assert_eq!(refusal(r1), PackRefusal::TooLarge);
    assert!(listing(&rig.nodes()).is_empty());

    let server = host(many);
    let mut rig = Rig::new(&origin(&server));
    let c = rig.candidate(&origin(&server));
    rig.installer.set_pack_limits(small.clone());
    let r2 = go(&rig, &c);
    assert_eq!(refusal(r2), PackRefusal::TooLarge);
    assert!(listing(&rig.nodes()).is_empty());

    // And the download itself.
    let noisy: Vec<u8> = (0..300_000u32)
        .map(|i| (i.wrapping_mul(2_654_435_761) >> 24) as u8)
        .collect();
    let server = host(noisy);
    let mut rig = Rig::new(&origin(&server));
    let c = rig.candidate(&origin(&server));
    rig.installer.set_pack_limits(small);
    let r3 = go(&rig, &c);
    assert!(r3.is_err());
    assert!(listing(&rig.nodes()).is_empty());
}
