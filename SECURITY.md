# Security Policy

## Supported Versions

Bushido Studio is pre-1.0. Fixes go into the current minor release only;
older releases are not patched.

| Version | Supported          |
| ------- | ------------------ |
| 0.1.x   | :white_check_mark: |

## Reporting a Vulnerability

Please do not open a public issue for a security problem.

Report it privately through GitHub's advisory form:
https://github.com/dcryptoRS/BushidoStudio/security/advisories/new

Include the version and the system information from Settings › About (it
has a copy button), your distribution, and steps to reproduce. A proof of
concept helps; a project file that triggers the problem helps most.

What to expect:

- An acknowledgement within a week.
- If the report is accepted: a fix in the next release, a published
  advisory, and credit to you unless you would rather not be named.
- If it is declined: an explanation of why.

Bushido Studio is built on [Concat](https://github.com/jub0t/Concat). A
problem that is in Concat's own code is passed on to Concat as well, so
both are fixed.

## What counts

Bushido Studio runs on your machine. Reports we care about most:

- A project file, media file, template or effect package that causes
  code execution, writes outside the project folder, or reads files it
  was not given.
- A downloaded model or its mirror being swapped for something else
  without the digest check catching it.
- The API server (Settings › Automation) accepting a call it should have
  refused: a missing or wrong token, on any address, or a server that
  started with no token at all.
- The Claude panel's guard letting through what it is there to stop
  (sudo, deleting whole folders, anything in the folders you protect).
- API keys for the generating services leaving the desktop's keyring or
  the file they fall back to.

By design, and not a vulnerability: the API server does whatever the
person who started it can do on that machine, and it is not encrypted.
Every connection presents a token, minted when none was set; binding to
anything other than 127.0.0.1 should sit behind something that provides
transport security. The Claude panel runs the Claude Code installed on
the machine, signed in to its owner's account, with the permissions the
panel describes.
