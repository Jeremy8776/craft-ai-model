## Problem

<!-- What is wrong or missing, and for whom. -->

## Change

<!-- What this PR does. One focused change. -->

## Verification

<!-- What you ran: fmt, clippy, tests for the crates touched, wasm check where a crate claims it.
Say plainly what was tested live, against a scripted server, or not at all. -->

## Checklist

- [ ] Tests written first; safety rules have a deliberate-bug check (`tools/mutation/`)
- [ ] No secrets in code, tests or logs
- [ ] Files stay under 500 lines (700 at most)
- [ ] Does not widen the shared contract without saying so
