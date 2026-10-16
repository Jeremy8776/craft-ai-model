# Contributing

Thanks for helping with the Craft AI layer. `README.md` says what
the crates are for and `docs/LAYOUT.md` says what lives where. Read both before a larger change.

## Branches and pull requests

Changes reach `main` through a pull request. Nothing is pushed to it directly.

1. Branch off `main`. Keep the branch short-lived and focused on one change.
2. Open a pull request into `main`. Explain what changed and why, and how you checked it.
3. Resolve every review conversation before merging.
4. Merge by squash or rebase. History stays linear, so merge commits are off.
5. The branch is deleted after merge. Delete your local copy too.

Review that matters happens at the boundaries: the shared contract goes to `craft-libs`, and each app
reviews its own adoption in its own repository. A change here should not quietly widen that contract.

## Commit messages

Use a conventional prefix with the crate in brackets where one crate is affected:

```
feat(ai-client): resolve the selected local backend and model
fix(ai-openai): stop retrying on a 4xx
docs: add LAYOUT.md
```

## Rules for every change

- **Test first.** Write the failing test, then the code. Ship tests with new logic, especially pure
  functions.
- **Safety rules get a deliberate-bug check.** Break the rule on purpose and confirm a test fails. No
  surviving mutant is ignored. The harness and its case lists are in `tools/mutation/`.
- **Small files.** Under 500 lines each, 700 at the very most. A file near the limit is doing too much,
  so split it by responsibility.
- **Keep `ai-core` small.** It uses serde only, with no I/O and no async runtime, and it must compile
  for wasm. Desktop-only code belongs in `ai-native`.
- **No unsafe code.** The workspace forbids it.
- **No secrets in the repo or in logs.** Keys go through the secret stores in `ai-native`.
- **Do not claim more than was run.** Say plainly what was tested live, what was tested against a
  scripted server, and what is only proposed.

## Building and testing

Builds are heavy, so keep them targeted to the crate you changed. CI runs the whole workspace.

```
cargo test -p <crate> -j 8
cargo clippy -p <crate> -j 8
```

Tests that touch the real OS keychain are `#[ignore]`d and run on request, for example
`cargo test -p craft-ai-native -- --ignored keychain_round_trip`. egui widgets need Rust 1.95.

## Licence

By contributing you agree that your work is licensed under MIT or Apache-2.0, at the option of the
user, matching `LICENSE-MIT` and `LICENSE-APACHE`.
