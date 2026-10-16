//! The pure rules of the model-source registry: which hosts a kind of source may use, which
//! file formats are installed, and what a person-supplied address means. No network, no disk.

use craft_ai_comfy_env::source::{
    Format, FormatProblem, SourceKind, SourceSpec, SpecError, host_matches, parse,
};

#[test]
fn a_host_pattern_matches_that_host_or_its_subdomains_and_nothing_longer() {
    assert!(host_matches("huggingface.co", "huggingface.co"));
    assert!(host_matches("huggingface.co", "HuggingFace.CO"));
    assert!(!host_matches("huggingface.co", "evilhuggingface.co"));
    assert!(!host_matches("huggingface.co", "huggingface.co.evil.com"));
    assert!(!host_matches("huggingface.co", "huggingface.co."));
    assert!(!host_matches("huggingface.co", "cdn.huggingface.co"));
    assert!(host_matches(".hf.co", "cdn-lfs.hf.co"));
    assert!(host_matches(".hf.co", "a.b.hf.co"));
    assert!(!host_matches(".hf.co", "hf.co"));
    assert!(!host_matches(".hf.co", "evilhf.co"));
    assert!(!host_matches(".hf.co", "hf.co.evil.com"));
    assert!(!host_matches("huggingface.co", ""));
    assert!(!host_matches("", "huggingface.co"));
}

#[test]
fn each_kind_names_its_own_hosts_and_which_one_may_see_a_token() {
    let hf = SourceKind::HuggingFace.rules();
    assert_eq!(hf.token_host, Some("huggingface.co"));
    assert!(hf.download.contains(&"huggingface.co"));
    assert!(hf.download.contains(&".hf.co"));
    let gh = SourceKind::GitHubRelease.rules();
    assert_eq!(gh.token_host, None, "public release assets need no token");
    assert!(gh.download.contains(&".githubusercontent.com"));
    let civitai = SourceKind::Civitai.rules();
    assert_eq!(civitai.token_host, Some("civitai.com"));
    assert!(SourceKind::Local.rules().download.is_empty());
    for kind in [
        SourceKind::HuggingFace,
        SourceKind::GitHubRelease,
        SourceKind::Civitai,
    ] {
        let rules = kind.rules();
        assert!(rules.token_host.is_none_or(|t| rules.download.contains(&t)));
        for pattern in rules.download {
            let bare = pattern.trim_start_matches('.');
            assert!(bare.contains('.') && !bare.contains('*'), "{pattern}");
        }
    }
}

#[test]
fn the_format_comes_from_the_extension_and_pickle_formats_are_named() {
    for (name, format) in [
        ("a.safetensors", Format::Safetensors),
        ("A.SafeTensors", Format::Safetensors),
        ("m.gguf", Format::Gguf),
        ("m.onnx", Format::Onnx),
        ("m.ckpt", Format::Pickle),
        ("m.pt", Format::Pickle),
        ("m.pth", Format::Pickle),
        ("m.bin", Format::Pickle),
        ("m.pkl", Format::Pickle),
        ("m.pickle", Format::Pickle),
        ("m.safetensors.pt", Format::Pickle),
        ("m.safetensors.exe", Format::Unknown),
        ("m.zip", Format::Unknown),
        ("model", Format::Unknown),
    ] {
        assert_eq!(Format::from_name(name), format, "{name}");
    }
    assert!(Format::Pickle.runs_code_when_loaded());
    assert!(!Format::Safetensors.runs_code_when_loaded());
    assert!(!Format::Gguf.runs_code_when_loaded());
}

fn safetensors_head() -> Vec<u8> {
    let mut head = 2u64.to_le_bytes().to_vec();
    head.extend_from_slice(b"{}");
    head
}

#[test]
fn the_bytes_are_read_not_just_the_name() {
    assert_eq!(Format::sniff(&safetensors_head()), Format::Safetensors);
    assert_eq!(Format::sniff(b"GGUF\x03\0\0\0"), Format::Gguf);
    assert_eq!(Format::sniff(b"PK\x03\x04rest"), Format::Pickle);
    assert_eq!(Format::sniff(&[0x80, 0x04, 0x95, 0, 0]), Format::Pickle);
    assert_eq!(Format::sniff(b"hello world"), Format::Unknown);
    assert_eq!(Format::sniff(b""), Format::Unknown);
    // A header that claims an absurd length is not a safetensors header.
    let mut absurd = u64::MAX.to_le_bytes().to_vec();
    absurd.push(b'{');
    assert_eq!(Format::sniff(&absurd), Format::Unknown);
}

#[test]
fn a_file_must_be_what_its_name_says() {
    assert_eq!(
        Format::check("m.safetensors", &safetensors_head()),
        Ok(Format::Safetensors)
    );
    assert_eq!(Format::check("m.gguf", b"GGUF\x03"), Ok(Format::Gguf));
    assert_eq!(Format::check("m.onnx", b"\x08\x07abc"), Ok(Format::Onnx));
    assert_eq!(Format::check("m.ckpt", b"anything"), Ok(Format::Pickle));
    assert_eq!(
        Format::check("m", b"x"),
        Err(FormatProblem::UnsupportedType)
    );
    // Pickle bytes under a safe-looking name, or text where weights should be.
    for name in ["m.safetensors", "m.gguf", "m.onnx"] {
        assert!(
            matches!(
                Format::check(name, b"PK\x03\x04rest"),
                Err(FormatProblem::Mislabelled {
                    found: Format::Pickle,
                    ..
                })
            ),
            "{name}"
        );
    }
    assert!(matches!(
        Format::check("m.safetensors", b"not weights at all"),
        Err(FormatProblem::Mislabelled { .. })
    ));
    assert!(matches!(
        Format::check("m.gguf", b"not weights at all"),
        Err(FormatProblem::Mislabelled { .. })
    ));
}

fn hf(repo: &str, file: &str, revision: &str) -> SourceSpec {
    SourceSpec::HuggingFace {
        repo: repo.into(),
        file: file.into(),
        revision: revision.into(),
    }
}

#[test]
fn hugging_face_addresses_become_a_repo_file_and_revision() {
    for url in [
        "https://huggingface.co/org/name/blob/main/dir/m.safetensors",
        "https://huggingface.co/org/name/resolve/main/dir/m.safetensors?download=true",
    ] {
        assert_eq!(
            parse(url),
            Ok(hf("org/name", "dir/m.safetensors", "main")),
            "{url}"
        );
    }
    let sha = "0123456789abcdef0123456789abcdef01234567";
    assert_eq!(
        parse(&format!(
            "https://huggingface.co/org/name/resolve/{sha}/a%20b.safetensors"
        )),
        Ok(hf("org/name", "a b.safetensors", sha))
    );
}

#[test]
fn github_release_and_civitai_addresses() {
    assert_eq!(
        parse("https://github.com/owner/repo/releases/download/v1.2/model.gguf"),
        Ok(SourceSpec::GitHubRelease {
            owner: "owner".into(),
            repo: "repo".into(),
            tag: "v1.2".into(),
            asset: "model.gguf".into(),
        })
    );
    for url in [
        "https://civitai.com/models/123?modelVersionId=456",
        "https://civitai.com/models/123/some-name?modelVersionId=456",
        "https://civitai.com/api/download/models/456",
        "https://civitai.com/api/download/models/456?type=Model&format=SafeTensor",
    ] {
        assert_eq!(
            parse(url),
            Ok(SourceSpec::Civitai { version_id: 456 }),
            "{url}"
        );
    }
    assert_eq!(
        parse("https://civitai.com/models/123"),
        Err(SpecError::NeedsVersion)
    );
}

#[test]
fn a_file_on_this_computer_must_be_an_absolute_path() {
    let path = std::env::temp_dir().join("m.safetensors");
    assert_eq!(
        parse(path.to_str().unwrap()),
        Ok(SourceSpec::Local { path: path.clone() })
    );
    assert_eq!(parse("m.safetensors"), Err(SpecError::NotSupported));
    assert_eq!(parse("../m.safetensors"), Err(SpecError::NotSupported));
    let climbing = std::env::temp_dir().join("..").join("m.safetensors");
    assert_eq!(parse(climbing.to_str().unwrap()), Err(SpecError::BadName));
}

#[test]
fn addresses_that_could_point_somewhere_else_are_refused() {
    let bad = [
        "http://huggingface.co/org/name/blob/main/m.safetensors",
        "https://user@huggingface.co/org/name/blob/main/m.safetensors",
        "https://huggingface.co.evil.com/org/name/blob/main/m.safetensors",
        "https://evilhuggingface.co/org/name/blob/main/m.safetensors",
        "https://example.com/m.safetensors",
        "ftp://huggingface.co/org/name/blob/main/m.safetensors",
        "https://huggingface.co/org/name/blob/main/../../x.safetensors",
        "https://huggingface.co/org/name/blob/main/%2e%2e/x.safetensors",
        "https://huggingface.co/org/name/blob/main/a%2Fb.safetensors",
        "https://huggingface.co/org/name/blob/main/a%5Cb.safetensors",
        "https://huggingface.co/org/name/blob/main//m.safetensors",
        "https://huggingface.co/org/name/blob/ma%20in/m.safetensors",
        "https://huggingface.co/org/name/blob/x%3By/m.safetensors",
        "https://huggingface.co/org/name/blob/main/.hidden",
        "https://huggingface.co/org/name/blob/main/c%3Aevil.safetensors",
        "https://huggingface.co/org/name/blob/main/m%00.safetensors",
        "https://huggingface.co/org/name",
        "https://huggingface.co/org/name/tree/main",
        "https://github.com/owner/repo/archive/main.zip",
        "https://github.com/owner/repo/releases/download/v1/",
        "https://github.com/owner/repo/releases/download/../x/m.gguf",
        "",
        "   ",
    ];
    for url in bad {
        assert!(parse(url).is_err(), "{url:?} should be refused");
    }
    let long = format!(
        "https://huggingface.co/org/name/blob/main/{}.safetensors",
        "a".repeat(3000)
    );
    assert!(parse(&long).is_err());
}

#[test]
fn the_kind_of_a_spec_says_whose_rules_apply() {
    assert_eq!(
        hf("a/b", "f.safetensors", "main").kind(),
        SourceKind::HuggingFace
    );
    assert_eq!(
        SourceSpec::Civitai { version_id: 1 }.kind(),
        SourceKind::Civitai
    );
    assert_eq!(
        SourceSpec::Local {
            path: std::env::temp_dir()
        }
        .kind(),
        SourceKind::Local
    );
}
