# Security policy

Shika is an early-stage, macOS-only project. Security fixes ship in the next
[release](https://github.com/xuanhieu2611/shika/releases); older releases are not
patched. Shika 0.3.0 and later offer updates through Sparkle; earlier versions
need the latest release downloaded by hand. Updates are verified with an EdDSA
signature and Shika's Developer ID before they install; see
[software updates](docs/updates.md).

## Reporting a concern

Open a [GitHub issue](https://github.com/xuanhieu2611/shika/issues) with a brief,
high-level description of the concern. Include the affected Shika commit or
version, your macOS version, and the affected CLI if relevant.

Issues are public. Do not include credentials, private code, personal paths,
unredacted terminal logs, or detailed exploit instructions. Use disposable
sample projects when describing a problem.

If demonstrating the concern requires sensitive information or exploit details,
open an issue asking the maintainer to arrange a private follow-up. Wait until a
private channel is agreed before sharing those details.

## Agent access

Shika launches coding-agent CLIs with automatic approval modes. A Git worktree
separates changes between tasks but does not restrict process access to files,
credentials, or the network. Review the
[agent permission model](README.md#supported-agents) before running Shika.

Configured worktree setup commands are trusted code and require local approval.
See [worktree preparation](docs/worktree-preparation.md) for the trust and
cleanup rules.
