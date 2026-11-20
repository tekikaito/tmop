# Security

## Reporting a vulnerability

Please report security problems privately, not in a public issue. Use
**[Report a vulnerability](https://github.com/tekikaito/tmop/security/advisories/new)** on the
repository's *Security* tab. Say which version of TMOP and which operating system you use, and how
to reproduce the problem.

## Supported versions

Fixes go into the newest release. Please check that the problem is still there in the
[latest release](https://github.com/tekikaito/tmop/releases/latest) before you report it.

## Verifying a download

Every file on a release page has a `.sha256` file next to it. The install commands check it for
you. To check by hand, compare the output of `sha256sum <file>` (Linux), `shasum -a 256 <file>`
(macOS) or `Get-FileHash <file>` (PowerShell) with that file.

TMOP's network use is described at the top of the [README](README.md).
