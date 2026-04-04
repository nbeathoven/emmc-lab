# Security Policy

## Supported Versions

Security fixes are applied on the latest published release line.

## Reporting a Vulnerability

Do not open a public GitHub issue for suspected vulnerabilities that could expose systems or data.

Instead, report privately through GitHub Security Advisories for this repository or contact the maintainer directly through the repository owner account.

When reporting, include:

- affected version or commit
- environment details
- reproduction steps
- impact description
- whether the issue requires local access, elevated privileges, or a specific kernel/device setup

## Scope Notes

This project operates close to storage devices and can run destructive raw-device workloads when explicitly authorized. Reports should distinguish between:

- intended destructive behavior after explicit confirmation
- missing or bypassed safety controls
- unintended data exposure
- privilege escalation
- insecure report storage or export behavior
