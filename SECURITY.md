# Security Policy

## Current security posture

SynthChat is a local desktop application. The Rust backend is intended to bind
only loopback addresses. `/health` is intentionally unauthenticated; protected
API routes require the random bearer token created by the desktop shell for the
current launch. The application is not a remote, TLS-terminated service and
must not be exposed through a public bind, reverse proxy, tunnel, or port
forward without a separate security design and review.

Provider and integration secrets are write-only application values backed by
the operating system credential store. They must not be placed in YAML, SQLite,
environment examples, source code, screenshots, issue reports, test snapshots,
or release artifacts. The desktop token is also secret: it is delivered over
stdin to the backend and must not be printed in command lines or logs.

Some approved tools can affect the host system. In particular, terminal and
code-execution approvals are not an OS or container sandbox and cannot roll
back external side effects. Treat them as local host authority. Consult
[`docs/terminal-process-contract.md`](docs/terminal-process-contract.md) before
changing those boundaries.

No release has yet established a signed, notarized, three-platform security
support matrix. The repository's current code-level and short local evidence is
recorded in [`docs/security-report.md`](docs/security-report.md); it is not a
release security certification.

## Reporting a vulnerability

Do not open a public issue, discussion, pull request, or chat message for a
suspected vulnerability.

Use the repository host's private security-advisory/reporting channel when it
is enabled. If it is unavailable, contact the project maintainer through the
private channel agreed for the deployment. State that the report is security
sensitive and wait for a private response before sending exploit details.

Include enough information to reproduce safely:

- affected version or commit and operating system;
- impact and attack preconditions;
- minimal reproduction steps or proof of concept;
- whether secrets, local files, process execution, or network exposure are
  involved;
- any mitigation already applied.

Redact tokens, API keys, bearer headers, credential-store exports, private user
content, databases, and crash dumps before sending them. Do not expect a
published response-time SLA until the project establishes a release support
policy.

## Handling a secret exposure

1. Revoke or rotate the exposed provider, MCP, update-signing, or other secret
   at its issuer immediately.
2. Remove the secret from active configuration and deployment systems.
3. Preserve enough private evidence for the maintainer to assess exposure, but
   do not copy it into tickets or commits.
4. Coordinate any Git history rewrite and remote force-update with maintainers
   before performing it. Do not delete shared history or local user data as an
   incident response shortcut.
5. Rebuild and scan the proposed release artifacts after remediation.

Historical credential remediation remains a release gate. Removing a file from
the current index does not remove a credential from Git history or distributed
artifacts.

## Release security gates

Before publishing a desktop artifact, the release owner must have current
evidence for all of the following:

- lockfile-resolved source build and tests;
- no tracked runtime databases, local data, `.env` files, legacy Agent runtime,
  or generated sidecar output;
- secret and token scanning of the source delta and final artifacts;
- real credential-store regression on each supported release platform;
- loopback/authentication/CORS and error-path log-redaction checks;
- native package installation, launch, update, and uninstall smoke testing;
- signing, notarization, provenance, and vulnerability-review evidence where
  the selected distribution channel requires them.

The current Windows/macOS/Linux packaging status and commands are deliberately
qualified in [`docs/release.md`](docs/release.md). Do not infer a signed or
cross-platform release from a successful source build.

