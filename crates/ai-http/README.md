# craft-ai-http

The one TLS setting every hosted call in Craft AI uses: certificates are checked by the platform's
own verifier, so a machine whose antivirus or company proxy re-signs HTTPS with an installed root
still works. The trade-off (those installed roots are trusted too) is written at the top of
`src/lib.rs`.

```rust
let config = ureq::Agent::config_builder()
    .tls_config(craft_ai_http::tls_config())
    .build();
```

A test fails if any crate in the workspace builds a `ureq` agent without it.
