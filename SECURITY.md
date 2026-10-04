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

### Automated dependency and code scanning

GitHub dependency graph and Dependabot security updates are enabled for every
manifest Vogt ships (`.github/dependabot.yml`). Pull requests run the
`dependency review` workflow, which blocks a change introducing a high or
critical severity dependency advisory. CodeQL scans Python, TypeScript,
JavaScript, and Rust on pull requests, pushes to `main`, and weekly.

`.github/dependabot.yml` carries two deliberate exceptions. The CodeQL
`init` and `analyze` actions are grouped into one update, because a PR that
bumps only one of them fails `analyze`. `ort` and `ort-sys` in `voice/` are
ignored: they are a matched pair pinned for `piper-rs`, a lone bump of either
does not build, and they are upgraded by hand together with the TTS stack.

Dependabot opens one PR per dependency, and `main` is rebase-only with strict
status checks, so landing a weekly wave one PR at a time means a serial
rebase-and-rerun per PR. Maintainers may instead cherry-pick the wave onto a
single branch, verify it once, and close the originals as superseded. Cancel
the closed PRs' queued CI runs, or they keep holding the self-hosted pool.

The scheduled `security alert triage` workflow reads open Dependabot and
CodeQL alerts and creates one labelled `security` issue per alert. The issue
contains the alert number as a stable marker, so reruns update the queue
without creating duplicates. Maintainers record remediation or disposition in
that issue before closing it.

After enabling these workflows, configure repository Settings → Rules → Rulesets
(or branch protection for `main`) with these required checks:

- `ci`
- `dependency review`
- `analyze (python)`
- `analyze (javascript-typescript)`
- `analyze (rust)`
- `runner-policy`

Also enable **Dependency graph**, **Dependabot alerts**, and **Dependabot
security updates** under Settings → Advanced Security. These are repository
settings and therefore cannot be represented in tracked files; verify them
after a repository transfer or visibility change.

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
- `runner-policy` also runs `scripts/check_workflow_policy.py`, which fails
  when a job (other than a reusable-workflow call) has no `timeout-minutes`,
  so a wedged job releases its runner instead of holding it for GitHub's
  360-minute default; and when a check in the required list above is not
  the name of some job that runs on `pull_request` or `merge_group`. A
  required context with no producer never reports, and every pull request
  blocks on it. That list is read from this file, so changing the ruleset
  means changing the list here in the same pull request.
- The scheduled `runner watchdog` workflow flags any run in progress longer
  than 150 minutes (above the longest job budget) in one open issue labelled
  `ci-stuck`, failing while anything is stuck and closing the issue once
  nothing is. It catches runs whose runner lost contact, which no job
  timeout ends.
- Secret-bearing steps (the Android keystore, the Firebase configuration)
  are gated on `github.event_name != 'pull_request'`, and no workflow uses
  `pull_request_target`.
- Release images are not built at release time: `release.yml` promotes, by
  digest, the images `build.yml` built on `main` (see *Release signing*
  below). Pull-request jobs never publish or sign an image.

If the approval setting is ever found disabled, treat every self-hosted runner
as potentially compromised by fork-submitted code and rotate the credentials
those runners can reach.

### Release signing: build once, promote by digest

Every published image is signed keylessly with cosign, by digest, using the
workflow's own GitHub OIDC identity — there is no signing key to store or
rotate. A released image carries two signatures on the **same digest**:

- `…/.github/workflows/build.yml@refs/heads/main`, made when `build.yml` built
  and smoke-tested it for a commit on `main`;
- `…/.github/workflows/release.yml@refs/tags/vX.Y.Z`, made when the `v*` tag
  promoted that digest. Deployments verify this identity
  ([`docs/DEPLOYMENT.md`](docs/DEPLOYMENT.md) §6).

`release.yml` never builds. Before it signs, it requires a `build.yml` run on
`main` for the tagged commit whose image jobs succeeded, and it verifies each
digest's `build.yml` signature with the certificate's workflow-sha pinned to
the tagged commit, so it can only promote what `build.yml` built from that
exact source. It retags by digest (`imagetools create --prefer-index=false`)
and asserts every semver tag resolves to the promoted digest. The SBOM and
provenance attestations are the ones BuildKit recorded in the image index at
build time; they travel with the digest. If any of this is missing the
release fails; there is no rebuild fallback.

**Accepted trade-off.** `build.yml` imports its BuildKit layer cache from the
operator's plaintext, unauthenticated LAN registry (`VOGT_BUILDKIT_CACHE_REGISTRY`)
to keep per-commit builds fast. Releases used to build cold precisely so that
a poisoned cache entry could not become a release-signed artefact; promoting
the `main` build gives that up in exchange for shipping exactly the bytes the
development lane ran. The cache is therefore part of the release trust
boundary: it must stay reachable only from the runner hosts, and the
fork-approval setting above is what keeps pull-request code from writing to
it. If either is in doubt, purge the cache repository and rebuild the release
commit on `main` before tagging.

### Reading runner state from an agent session

Agents diagnosing a stalled pool need to list the organisation's self-hosted
runners and whether each is online or busy (`GET /orgs/{org}/actions/runners`,
or `gh api orgs/<org>/actions/runners`). A token without the organisation
permission **Self-hosted runners: read** gets HTTP 403, which is what agent
sessions have been hitting. The grant is an organisation setting, not
something the tree can carry: a fine-grained token (or the GitHub App the
agents act as) needs **Organization permissions → Self-hosted runners →
Read-only** added by an organisation owner. Read-only is sufficient and is
the most that should be granted; *Read and write* would let a session
register or remove runners. Listing workflow runs and jobs, which the
watchdog does, needs only repository **Actions: read** and is unaffected.
A classic personal access token can only reach this endpoint through the
`manage_runners:org` scope, which also grants write; prefer a fine-grained
token for agent sessions.

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
