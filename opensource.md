# Vogt open-source delivery direction

This document records the product direction agreed for the open-source
transition of Vogt. It is a working brief for the implementation and
documentation pass that follows; it is not a replacement for the product
requirements, design, or deployment documents. The requirements document,
including its revision history, comments, delivery checks, and gap register,
remains authoritative for the product's functional intent.

## Context

Vogt is being prepared for a generic, self-hostable open-source delivery. The
current repository grew inside a private estate and therefore contains two
different kinds of material in one tree:

1. the reusable Python product core; and
2. private deployment and development-environment assumptions, including the
   merged Rust session engine, personal filesystem paths, private services,
   and agent integrations.

The open-source work must separate those concerns cleanly. A public user
should be able to understand, build, run, and deploy Vogt without knowing the
original estate or its private services. At the same time, the existing
deployment must remain reproducible and operable for the owner while this
transition is made.

## Decisions already made

### 1. Vogt is the only public product identity

All `MyDevEnv2` naming and other inherited product identifiers are to be
removed from the public delivery. This includes, where the relevant surface
is retained, executable names, Rust/Python package and crate-facing names,
environment-variable prefixes, mobile identifiers, browser storage/event
keys, paths, service names, examples, comments, and user-facing
documentation.

The migration should be deliberate rather than a search-and-replace. Names
that are persisted or sent over a wire need an explicit compatibility and
migration decision. Where existing deployments depend on an old name, a
temporary compatibility alias or migration must be provided and documented;
new public examples must use Vogt names only.

The canonical repository and public source identity are:

`https://github.com/TheDancingDeveloper-org/vogt`

The canonical live development/example endpoint for documentation and smoke
testing is:

`https://vogt-dev.sprooty.com/`

That URL is an example deployment identity, not a default that an image or
local installation should silently assume. Generic examples must require an
operator to set their own public URL when it is an exposure decision.

### 2. The public distribution is Python-core-first

The supported public container artefact is the Python core image built from
the repository-root `Dockerfile`. The public quickstart, sample image, sample
Compose file, release instructions, and compatibility tests must all work
with that image alone.

The Rust session engine and its PWA/mobile delivery are not part of the
generic public container path for this transition. They may remain in the
repository as a separately documented/private or future component, but the
public core package must not require Rust, Cargo, Node, pnpm, the embedded PWA,
or the engine's development pod to build or run Vogt's core functionality.

This is a product boundary, not a claim that the engine has no value. The
boundary makes the open-source installation understandable and reproducible;
the current merged estate can continue to use its private deployment
configuration while the separation is completed.

### 3. The current deployment must remain possible

The transition must not strand the existing live stack. The current setup
must remain deployable by applying configuration in the same way an end user
would apply the public examples: from declared image/configuration artefacts,
not by hand-editing a running container or relying on undocumented host state.

If preserving the current merged Rust stack requires private configuration,
private image/workflow settings, legacy aliases, or a private branch, that is
acceptable. Those details must be isolated and clearly labelled as private
deployment material rather than presented as the generic open-source
quickstart.

The desired separation is therefore:

- **Public path:** generic Python-core image and self-contained Compose
  example, suitable for a new operator on a normal Docker host.
- **Private/current path:** the existing estate deployment, including any
  Rust engine, session tooling, Tailscale/Komodo/Infisical integration, and
  owner-specific mounts or secrets, retained where needed to keep the live
  stack like-for-like.

Both paths must be tested as configuration-driven deployments. Publishing an
image must remain distinct from deploying it; a digest/configuration change is
what moves a deployment.

### 4. Cadastre and MCP are optional integrations

Cadastre MCP must not be a hard dependency of Vogt. In particular, the public
image and the public Compose example must not require:

- a Cadastre checkout;
- the `cadastre[mcp-client]` package or `cadastre-mcp-remote` executable;
- a private Cadastre endpoint or token;
- Infisical or another private secret broker solely to initialise MCP; or
- a configured MCP client in order for Vogt's core CLI, REST API, health
  checks, storage, or GUI to start.

MCP remains an optional adapter/integration where it is useful. It may be
installed as an explicit extra, run as a separate process/profile, or be
configured by an operator who wants agent tooling. A Vogt installation with
MCP disabled must still be a complete, supported product for the core use
cases.

The same rule applies to Cadastre specifically: its absence must be an honest
and non-fatal optional-integration state, not a startup failure, failed health
check, or empty or misleading core product. Private estate bootstrap scripts
may continue to register Cadastre when explicitly enabled, but they must not
be in the public image's default build or startup path.

### 5. Public deployment examples must be self-contained

The repository will provide:

- a sample/buildable Docker image for the Python core;
- a sample Docker Compose file that can run that image on a generic host;
- an example environment/configuration file with safe, understandable
  placeholders and allocation defaults where appropriate; and
- a clear getting-started guide that takes a new operator from checkout or
  published image to a working instance.

The sample Compose file must not encode the current private estate. It must
not depend on personal absolute paths, Node B addresses, Komodo, Infisical,
Tailscale, private registries, private DNS, private certificates, mounted
home directories, or a Cadastre service. State should use a named volume or a
clearly explained operator-owned path. Exposure values (for example a public
URL or externally published address) must be explicit; the example must not
pretend to infer them.

The sample must preserve the current hardening and operational guarantees
that make sense for the core image: non-root execution, persistent data,
health checks that do not require an MCP handshake, clear token/bootstrap
behaviour, and a documented backup/upgrade path.

### 6. Documentation is being rewritten for a public reader

This is a full documentation update pass, not a README patch. The public
documentation should explain:

- what Vogt is and what the Python core provides;
- prerequisites and supported installation paths;
- the Docker image and Compose quickstart;
- local, CLI, REST, GUI, and optional MCP usage;
- configuration, authentication, data storage, backup, upgrades, and
  troubleshooting;
- optional GitHub/forge integrations and their failure/absence semantics;
- how to contribute and run the test/tooling checks; and
- which engine/mobile material is private, optional, future, or otherwise
  outside the core quickstart.

The result should be internally consistent: links, commands, image names,
environment variables, service names, and examples must describe the same
product and the same supported path. Generated configuration documentation
must continue to come from `src/vogt/config.py`; it must not be hand-edited
to hide a schema change.

The requirements document is deliberately excluded from the rewrite that
would erase history. `docs/REQUIREMENTS.md` must retain its complete
requirements list, revision notes, explanatory comments, delivery
verification, and §7 gap register. If a requirement or architecture decision
changes as part of this transition, record the change there with a new
revision rather than deleting the rationale that led to the earlier choice.
Historical merge/deployment material can remain as history, but private
details must be labelled as historical/private and must not be the only
instructions a public user has for running Vogt.

## Intended implementation shape

The work should be performed in coherent layers:

1. **Define the public boundary.** Identify the Python-core artefacts and
   supported surfaces. Mark the Rust engine/mobile and private estate pieces
   as separate deployment material until they have an explicit public
   product decision.
2. **Make the core independently distributable.** Ensure the root Dockerfile,
   Python package, CLI, REST server, health checks, storage, and GUI run
   without engine or MCP/Cadastre installation.
3. **Make optional integrations explicit.** Move Cadastre/MCP setup behind
   opt-in extras, profiles, or operator configuration. Do not silently install
   or register them during image build or container startup.
4. **Replace private deployment examples.** Add generic image/Compose/env
   examples and preserve the current estate deployment separately, with
   explicit compatibility notes and migration steps where names or paths
   change.
5. **Rename the retained public surfaces.** Remove inherited MyDevEnv2
   identity from new runtime and documentation surfaces, while providing
   migrations/aliases for persisted or live-deployment identifiers where
   needed.
6. **Rewrite and verify the docs.** Add a dedicated getting-started guide,
   update the public README and operational references, preserve the complete
   requirements history, and run link/config/doc checks plus core tests.

## Acceptance criteria

The transition is complete only when all of the following are true:

- A new user can build the Python-core image from this public repository
  without access to private registries, paths, services, or repositories.
- A new user can run the sample Compose file with only documented local
  configuration and a persistent data volume.
- The core starts, serves health/API/UI endpoints, and supports its primary
  workflows with MCP disabled and Cadastre absent.
- No public Dockerfile, Compose example, startup script, or getting-started
  command installs or assumes `cadastre-mcp-remote`.
- Public examples no longer use MyDevEnv2 or other private estate identity.
- Any retained legacy names have a documented compatibility or migration
  story, especially for persisted data, environment variables, URLs, and
  client configuration.
- The existing current stack remains reproducible through explicit private
  configuration, without manual container edits.
- Documentation names `https://vogt-dev.sprooty.com/` only as the known
  development/example deployment and never as a hidden local default.
- `docs/REQUIREMENTS.md` remains complete, including comments and historical
  rationale, with any new transition decision recorded as a revision.
- Python tests, coverage, type/lint checks, generated-config checks, and
  documentation link checks pass for the public core path.

## Open implementation questions

These are intentionally left for the implementation pass rather than being
silently decided here:

- whether optional MCP support belongs in a Python optional dependency,
  separate package, or an explicitly documented built-in module that is not
  installed/started by default;
- the exact compatibility window for old `MYDEVENV2_*` names and persisted
  engine identifiers in the private deployment;
- whether the Rust engine remains in this repository, moves to a private
  branch/repository, or is later reintroduced as a separately installable
  optional component; and
- the final public image publication/tagging policy, provided it targets the
  repository and organization stated above and remains digest-verifiable.

Until those questions are resolved, new public artefacts should follow the
Python-core boundary and must not add further private coupling.
