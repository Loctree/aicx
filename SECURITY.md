# Security Policy

## Reporting a Vulnerability

If you discover a security vulnerability in aicx, please report it responsibly.

**Do not open a public issue.**

### How to Report

- Use [GitHub Security Advisories](https://github.com/Loctree/aicx/security/advisories/new) to submit a private report.
- Alternatively, contact us directly at **security@loctree.com**.

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

## Dashboard HTTP Defense Model

The dashboard HTTP surface is defended by a deliberate, layered model. This
is a conscious design decision, not an accidental omission.

- **Primary defense — auth token.** Every dashboard route is gated by a
  256-bit CSPRNG auth token, compared in constant time. The token is never
  logged and never echoed into UI/HTML or error bodies; failures return a
  uniform `401` with no oracle.
- **Primary defense — explicit non-loopback policy.** The server refuses to
  bind to a non-loopback address unless the operator has explicitly opted in
  to a non-local CORS policy *and* enforced auth is active. Loopback-only is
  the default.
- **Defense-in-depth — CSRF/Origin checks.** The older CSRF gate is retained
  as defense-in-depth (Origin/header validation on mutating routes), but it is
  no longer the primary model. Because the dashboard uses no cookie/session
  ambient credential, classic CSRF is structurally absent regardless.

### Accepted risks (operator sign-off)

- **Auth token in process argv.** When the dashboard is started via the
  background-spawn path, `--auth-token` is passed as a child-process argument,
  so it is visible to other local users via `ps` / `/proc/<pid>/cmdline`. On a
  single-operator host this is accepted; on shared hosts prefer
  `AICX_HTTP_AUTH_TOKEN`.
- **Rate limiting is peer-IP, not proxy-aware.** The `/api/*` rate limiter
  buckets by peer IP. Behind a reverse proxy (nginx/Caddy) all clients share
  one bucket per proxy IP; proxy-aware limiting is a tracked follow-up.

---

Vibecrafted with AI Agents by Vetcoders (c)2026 Vetcoders
