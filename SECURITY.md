# Security Policy

## Supported Versions

| Version | Supported          |
|---------|--------------------|
| 0.1.x   | :white_check_mark: |

## Reporting a Vulnerability

If you discover a security vulnerability in W3C OS, please report it responsibly.

**Do NOT open a public GitHub Issue for security vulnerabilities.**

Instead, please email: **wangnaihe@chemanman.com**

Include the following in your report:

- Description of the vulnerability
- Steps to reproduce
- Potential impact
- Suggested fix (if any)

## Response Timeline

- **Acknowledgement**: within 48 hours
- **Initial assessment**: within 7 days
- **Fix or mitigation**: within 30 days for critical issues

## Scope

The following are in scope:

- All code in the `crates/` directory
- Build scripts and CI/CD pipelines
- The bootable ISO and system scripts in `system/`
- The Dockerfile and DevContainer configuration

The following are out of scope:

- Third-party dependencies (please report to the upstream project)
- Social engineering attacks
- Denial of service attacks against GitHub infrastructure

## Disclosure Policy

We follow coordinated disclosure. We will:

1. Confirm the vulnerability and determine its impact
2. Prepare a fix
3. Release a patched version
4. Credit the reporter (unless they prefer anonymity)

Thank you for helping keep W3C OS secure.
