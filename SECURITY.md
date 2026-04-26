# Security policy

## Reporting a vulnerability

If you find a security issue in Coulisse, please report it privately rather than
opening a public issue. Use GitHub's [private vulnerability reporting][gh-pvr]
on this repository, or email **almaju.fr@gmail.com** with details and (if
possible) a reproduction.

You should expect an initial response within a week. Once a fix is available, it
will be released and credited in the changelog.

## Scope

Coulisse proxies traffic to LLM providers and stores per-user conversation
history and API credentials in `coulisse.yaml`. Reports about credential
handling, request smuggling, auth bypass, SQL injection, or path traversal in
the admin UI are in scope. Reports about misconfiguration of a deployment
(weak admin passwords, exposed `coulisse.yaml`, etc.) are out of scope.

## Supported versions

Only the latest release receives security fixes during the pre-1.0 phase.

[gh-pvr]: https://github.com/Almaju/coulisse/security/advisories/new
