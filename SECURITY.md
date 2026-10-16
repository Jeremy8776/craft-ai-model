# Security

This repository handles API keys, local model downloads, files fetched from the internet and the
agreement a person gives before their data leaves the machine. Please report problems privately.

## Reporting a vulnerability

Use GitHub's private reporting: open the **Security** tab of this repository and choose
**Report a vulnerability**. Please do not open a public issue or pull request for it.

Include what you found, the crate and version or commit, and the smallest steps that reproduce it.
We will acknowledge the report, tell you what we plan to do and credit you if you wish.

## What counts

Examples of what we want to hear about:

- A key, token or other secret written to a file, a log, an error message or the shared store.
- Sending data to a provider without the person's agreement, or reusing an agreement beyond its
  app, connection, destination, kind of data or expiry.
- A downloaded archive or file that escapes its folder, overwrites something it should not, or runs.
- A request that follows a redirect or address it should refuse (for example to a private network).
- A TLS check that is weaker than documented.

## Supported versions

The crates are pre-1.0 and nothing here is a final API. Fixes go to `main`.
