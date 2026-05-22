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

Generated auth tokens are persisted only on Unix platforms, where `aicx` sets
`~/.aicx/auth-token` to mode `0600` after writing it. Windows token-file
persistence is refused before the token file is written because this build does
not configure a restricted Windows DACL.

On Windows, pass a token explicitly with `--auth-token <token>` or use
`AICX_HTTP_AUTH_TOKEN` so no token file is created.

---

Vibecrafted with AI Agents by VetCoders (c)2026 VetCoders
