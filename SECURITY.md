# Security Policy

## Supported Scope

Security reports may cover:

- Authentication, authorization, tenant isolation, and permission bypass.
- Secret storage, API key encryption, token leakage, and log redaction.
- Agent runtime command execution, workspace isolation, path traversal, and
  process cancellation.
- Bot Gateway inbound verification, replay/deduplication, outbound delivery, and
  platform credential handling.
- NL2SQL datasource access, SQL safety, masking, and execution permissions.

## Reporting A Vulnerability

Please do not open a public issue for suspected vulnerabilities.

Use one of these channels:

- GitHub's **Security → Report a vulnerability** private reporting flow.
- Private maintainer contact only when private vulnerability reporting is not
  available on the repository.

Include:

- Affected version or commit.
- Reproduction steps.
- Impact and affected component.
- Logs/screenshots with secrets redacted.
- Whether the issue is already public.

We aim to acknowledge reports within 72 hours after the project has a public
maintainer address.

## Secret Handling

- Never commit `.env`, API keys, bot tokens, database dumps, runtime artifacts,
  private workspaces, or logs containing credentials.
- Use `.env.example` only for placeholders.
- AOS encrypts table-backed API keys, but operators must still rotate leaked
  keys immediately.
- Configure reverse proxies and access-log pipelines to omit or redact query
  strings on WebSocket endpoints; browser WebSocket authentication may carry a
  short-lived credential in the query string.

## Runtime Security Notes

The default local-process runtime is intended for local development and trusted
operator environments. It can execute commands in task workspaces. Production
operators should:

- Use least-privilege OS users.
- Keep workspaces under `AOS_DATA_DIR`.
- Review command allowlists and timeouts.
- Prefer sandboxed runtime profiles when available.
- Never mount sensitive host directories into Agent workspaces.
