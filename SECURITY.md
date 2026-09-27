# Security Policy

Mouzi watches folders and automatically moves files, so security and predictable file handling matter. This document covers **this fork**, `g2gonee2025-cloud/mouzi`. The upstream project at `hsr88/mouzi` has its own policy and its own maintainer; reports about unmodified upstream behaviour should go there.

## Scope

This fork's release is 0.2.0. It is a locally built, unsigned, single-user application. There is no code signing, no updater, and no update-check endpoint, so you are responsible for obtaining and trusting the build you run.

The fork adds attack surface beyond upstream. Reports in any of these areas are especially welcome:

| Area | What it involves |
|------|------------------|
| Archive extraction | Importing `.zip`, `.tgz`, and `.tar.gz` archives from Takeout, including path traversal, zip bombs, and symlink handling |
| File moves | Cross-device moves, destination name uniquification, and time-of-check to time-of-use races between the lock check and the move |
| Recycle Bin operations | Sending files to trash via the `trash` crate, and whether a path can be redirected before trashing |
| Local LLM call | Sending file names to a locally running Ollama instance on `localhost:11434`, and whether the request can be redirected off the loopback interface |
| Inventory and cleanup | The recursive scanner, symlink and NTFS junction cycle handling, and the duplicate, large, stale, and empty-directory cleanup paths |
| SQL and paths | Building queries from user-controlled paths, names, or rule fields |

The upstream areas still apply: file paths, symbolic links, rule processing, unintended file operations, privilege boundaries, and code execution.

## Supported versions

There is no updater in this fork, and no automatic patching. The only released version is 0.2.0, and it is the only version this policy covers. Earlier or later builds are unsupported.

Because there is no update channel, a fix is not going to reach you automatically. If you run a build you produced yourself, rebuild from source after pulling the fix.

## Reporting a vulnerability

Please do not report security vulnerabilities through public GitHub issues, discussions, or social media.

Report privately to the **fork maintainer** using GitHub's private vulnerability reporting feature on this repository:

[Report a vulnerability privately](https://github.com/g2gonee2025-cloud/mouzi/security/advisories/new)

Do not send reports to `hsr88/mouzi`, which is a different project with a different maintainer.

Please include:

* A clear description of the vulnerability.
* The affected version, commit, and operating system.
* Steps to reproduce.
* The potential impact.
* Proof-of-concept files or code, if applicable.
* Any suggested fix or mitigation.
* For anything involving the Ollama path: whether a non-loopback endpoint could be substituted, and how.

You should receive an initial response within seven days. Fix and disclosure timelines depend on severity and complexity. Please allow reasonable time for the issue to be investigated and fixed before publishing details.

## Safe research

Good-faith security research is welcome when it:

* Is performed on systems and files you own or are authorised to test.
* Avoids accessing other people's data.
* Does not disrupt services or distribute malicious files.
* Gives the maintainer a reasonable opportunity to address the issue.

There is no paid bug bounty programme. Reports are still appreciated, and researchers may be credited if they wish.
