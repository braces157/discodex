# Security

## Reporting a vulnerability

Please use GitHub's private vulnerability reporting / Security Advisory flow for issues that could expose local files, conversation content, credentials, or other sensitive data.

Do not open a public issue with secrets, session contents, tokens, private prompts, or unredacted personal data.

For ordinary bugs that do not involve sensitive data, use the public bug report template.

## Scope

Security-sensitive areas include:

- Local session-log parsing
- Windows foreground-process/window inspection
- Discord IPC payload construction
- Runtime configuration handling
- Windows startup registration

Reports should include the affected Discodex version, Windows version, reproduction steps, and the minimum redacted evidence needed to demonstrate the problem.

