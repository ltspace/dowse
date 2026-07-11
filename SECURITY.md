# Security Policy

## Privacy stance

dowse runs entirely on your machine. The index is stored locally (`%LOCALAPPDATA%\dowse`), file contents and OCR output never leave the device, and the application makes no network calls and collects no telemetry. There is no server component to breach and no account to compromise — the attack surface is the local binary and the local index.

## Reporting a vulnerability

If you find a security issue (e.g. a path traversal, a crash triggerable by a crafted file, unsafe handling of untrusted input during indexing or OCR), please report it privately through GitHub's [private vulnerability reporting](https://docs.github.com/en/code-security/security-advisories/guidance-on-reporting-and-writing/privately-reporting-a-security-vulnerability) on this repository (Security tab → "Report a vulnerability"), rather than opening a public issue.

Please include steps to reproduce and, if possible, the smallest input that triggers the problem. We will acknowledge reports and follow up with a fix timeline once triaged.
