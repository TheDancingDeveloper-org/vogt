# Security policy

## Supported versions

Vogt is pre-1.0 and moves fast. Security fixes are made against the latest
released minor version only; there is no long-term-support branch yet.

| Version | Supported          |
| ------- | ------------------ |
| 0.2.x   | :white_check_mark: |
| < 0.2   | :x:                |

Once the project reaches 1.0, this table will state a real support window.

## Reporting a vulnerability

Please use **GitHub's private vulnerability reporting** rather than a public
issue: open the repository's **Security** tab and choose **"Report a
vulnerability"**. That creates a private advisory only the maintainer (and
anyone they add) can see, and lets you attach reproduction steps or a patch
without exposing the issue while it is unfixed.

If you cannot use that flow, do not publish exploit details in an issue. The
project does not currently have an email or another private fallback channel;
establishing and exercising one remains release work. In a private report,
include:

- what you found and why it is a security issue, not just a bug;
- steps or a proof-of-concept to reproduce it;
- the version or commit you tested against;
- any suggested fix, if you have one.

You should get an acknowledgement within a few days. There is no bug-bounty
program; this is a self-hosted personal/small-team project, and the
maintainer's capacity is limited. Please give a reasonable amount of time to
fix a confirmed issue before any public disclosure.

## Vogt's security model, so you know what "a vulnerability" means here

Vogt is a self-hosted product; there is no shared multi-tenant instance to
worry about, but a single misconfigured or compromised instance can still
expose real project data. The model, in short (the full statement is in
[`docs/DEPLOYMENT.md`](docs/DEPLOYMENT.md) §4.1 and
[`docs/DESIGN.md`](docs/DESIGN.md)):

- **Scoped bearer tokens.** Every request is authenticated by default
  (`--no-auth` exists only for a loopback listener). A token is bound to an
  actor and carries scopes (`read`, `work.write`, `project.write`, `admin`,
  `writeback`), minted with `vogt token issue` and shown once. Core-side
  tokens are configured as `*_file` paths. The optional engine's generic
  Compose overlay currently accepts `ENGINE_TOKEN` in its ignored `.env` for
  compatibility; operators who need to exclude it from `docker inspect` can
  use the engine TOML `token` setting or a private secret-backed overlay.
- **Audited writes.** Every mutating operation requires a principal and a
  reason and lands its entity change, audit row, and event row in one
  transaction (`audited_write`). There is no write path that bypasses this.
- **An optional GitHub adapter.** Vogt can read (and, if configured, write
  back to) GitHub using either a single file-based token
  (`VOGT_GITHUB_TOKEN_FILE`) or per-actor linked personal access tokens,
  encrypted at rest with a Fernet key (`VOGT_FORGE_ACCOUNT_KEY_FILE`). Absent
  configuration disables the adapter rather than degrading it insecurely:
  forge data reads as "not collected", not as empty-and-trusted.

Things worth a report under this model: a way to read or write data without a
valid token and the right scope, a way to forge or replay a token, a write
that lands without an audit row, a way to make the optional GitHub adapter
leak a token (file-based or linked) to an unintended party, or a way to
escalate scopes. Missing rate limiting on a self-hosted single-operator
service, or the debug-signed dev/CI build artifacts being unsigned, are known
and out of scope unless you can show real impact.

## Mobile Firebase configuration

`mobile/android/app/google-services.json` is operator-supplied and must never
be committed. It is ignored by Git. The tracked
`mobile/android/app/google-services.json.example` is a sanitized fixture for
forks and pull requests; it does not provide working Firebase or FCM access.

Trusted Android builds fetch their real configuration from Infisical and remove
the working file after Gradle finishes. Pull-request builds use the sanitized
fixture and do not receive the Infisical credentials.

The CI `tracked secret hygiene` job checks every reviewed tree for the live
configuration filename and Firebase-looking API keys. It intentionally checks
the current tree only: removing a credential from history requires an
operator-coordinated rewrite and cannot be performed by an ordinary pull
request.

## If a credential is exposed

1. Revoke or rotate it in Google Cloud/Firebase immediately. Restrict any
   replacement to the intended Android package names and signing certificates.
2. Decide whether the repository history must be rewritten. Rotation makes the
   old value unusable; it does not remove old commits from clones, tags, or
   hosting caches.
3. Provision the replacement configuration outside Git and store it in the
   configured secret manager.
4. Run a full-history secret scan after the operator action and confirm that
   current-tree CI remains green.

Do not paste credentials into issues or pull requests. Report a suspected new
exposure privately to the repository maintainers.
