# Security Policy

## Reporting a Vulnerability

If you discover a security vulnerability in aicx, please report it responsibly.

**Do not open a public issue.**

### How to Report

- Use [GitHub Security Advisories](https://github.com/VetCoders/ai-contexters/security/advisories/new) to submit a private report.
- Alternatively, contact us directly at **void@div0.space**.

### What to Include

- Description of the vulnerability.
- Steps to reproduce.
- Potential impact.

### Response Timeline

- **Acknowledgment:** within 72 hours of report.
- **Assessment:** we will evaluate severity and confirm the issue.
- **Fix:** a patch will be developed and tested before any public disclosure.
- **Disclosure:** coordinated with the reporter. No public disclosure before a fix is available.

## Supported Versions

Security fixes are applied to the latest release on the `main` branch.

## Auth Token Storage

Generated auth tokens are persisted only on Unix platforms, where `aicx`
creates `~/.aicx/auth-token` atomically with mode `0600` via
`OpenOptions::create_new(true).mode(0o600)`. There is no window between
open and chmod — the file is unreadable to non-owner from the moment it
exists. Windows token-file persistence is refused before the token file
is written because this build does not configure a restricted Windows
DACL.

On Windows, pass a token explicitly with `--auth-token <token>` or use
`AICX_HTTP_AUTH_TOKEN` so no token file is created.

---

Vibecrafted with AI Agents by VetCoders (c)2026 VetCoders
