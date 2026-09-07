# Security policy

## Supported versions

Vogt is pre-1.0 and moves fast. Security fixes are made against the latest
released minor version only; there is no long-term-support branch. Releases
are `v*` tags on `main`.

| Version | Supported          |
| ------- | ------------------ |
| 0.6.x   | :white_check_mark: |
| < 0.6   | :x:                |

## Reporting a vulnerability

Please use **GitHub's private vulnerability reporting** rather than a public
issue: open the repository's **Security** tab and choose **"Report a
vulnerability"**. That creates a private advisory only the maintainer (and
anyone they add) can see, and lets you attach reproduction steps or a patch
without exposing the issue while it is unfixed.

If you cannot use that flow, do not publish exploit details in an issue. The
project has a single maintainer and no email or other private fallback
channel; GitHub's private reporting is the route. In a private report,
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
expose real project data. The model, in short (the full statement is in the
tokens section of [`docs/DEPLOYMENT.md`](docs/DEPLOYMENT.md) and in
[`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md)):

- **Scoped bearer tokens.** Every request is authenticated by default
  (`--no-auth` exists only for a loopback listener). A token is bound to an
  actor and carries scopes (`read`, `work.write`, `project.write`, `admin`,
  `writeback`), minted with `vogt token issue` and shown once. Core-side
  tokens are configured as `*_file` paths, and the stack brokers the core
  token to both halves as a file secret. The engine's own bearer token,
  `ENGINE_TOKEN`, is read from the git-ignored `deploy/.env` as an
  environment value and is therefore visible in `docker inspect`; operators
  who need it out of the environment can set it in the engine's TOML `token`
  setting or through a secret-backed overlay of their own.
- **A development pod, by design.** The stack image carries a writable home,
  passwordless `sudo`, an SSH server, a Docker CLI, and the agent CLIs,
  because it exists to run arbitrary agent sessions; it cannot be hardened
  against the sessions it runs. The Compose file publishes it on loopback
  unless `ENGINE_BIND` says otherwise, and the core inside it listens on
  loopback only — the entrypoint refuses to start if `VOGT_CORE_URL` names
  anything else. Exposing it means a real address in `ENGINE_BIND` and
  something that terminates TLS in front.
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
service, or debug-signed CI build artifacts being unsigned, are known and out
of scope unless you can show real impact.

## CI and self-hosted runners

This is a public repository, and `pull_request`-triggered jobs run on the
project's self-hosted runner pool (`ci.yml`, `codeql.yml`,
`runner-policy.yml`, `docs.yml`, `mirror-base-images.yml`). Those jobs run
`uv`/`pytest`, `pnpm install`, `cargo`, Playwright and `gradlew` over
PR-controlled source — arbitrary code execution by design — on a pool that
also runs the secret-bearing jobs.

**The single load-bearing control is the repository/organisation setting
"Require approval for _all_ outside collaborators" (Settings → Actions → Fork
pull request workflows).** GitHub's default requires approval only for
first-time contributors, which is not sufficient here: one merged typo fix
would otherwise let a fork author run code on the pool automatically. This
setting must stay enabled; it is not enforceable from the tree, so it is
stated here to be audited. The exposure is closed at that approval gate, not
by where the pipeline runs. What the tree _does_ enforce:

- `runner-policy` asserts every job names a self-hosted runner and none is
  selected dynamically, and that every third-party action is pinned to a
  full commit SHA. It reports rather than blocks, so it must also be a
  required status check on `main` — another setting to audit alongside the
  approval gate.
- Secret-bearing steps (the Android keystore, the Firebase configuration)
  are gated on `github.event_name != 'pull_request'`, and no workflow uses
  `pull_request_target`.
- Signed release images build without importing a shared layer cache, so
  nothing a pull-request job writes to a cache can reach a signed artifact
  (`release.yml`).

If the approval setting is ever found disabled, treat every self-hosted runner
as potentially compromised by fork-submitted code and rotate the credentials
those runners can reach.

## Mobile Firebase configuration

`mobile/android/app/google-services.json` is operator-supplied and must never
be committed. It is ignored by Git. The tracked
`mobile/android/app/google-services.json.example` is a sanitised fixture for
forks and pull requests; it does not provide working Firebase or FCM access.

Builds that are not pull requests write the real configuration from a
repository secret and remove the working file after Gradle finishes.
Pull-request builds use the sanitised fixture and receive no secret.

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
3. Provision the replacement configuration outside Git, as a CI secret.
4. Run a full-history secret scan after the operator action and confirm that
   current-tree CI remains green.

Do not paste credentials into issues or pull requests. Report a suspected new
exposure privately to the maintainer through the vulnerability-reporting flow
above.
