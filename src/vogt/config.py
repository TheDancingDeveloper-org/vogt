"""Configuration — the single source of truth (NFR-Q4).

`docs/CONFIG.md` and `config.example.toml` are *generated* from the schema
below by `scripts/gen_config_docs.py`, and CI fails when the committed files
drift from it. A setting documented in one place and implemented in another
will eventually disagree; here there is only one place.

Precedence, highest first: explicit arguments, `VOGT_*` environment
variables, the TOML file named by `VOGT_CONFIG_FILE`, then the schema
defaults.

## The default policy (NFR-D2, revised r4)

Every field declares what its value *decides*, because that — not whether it
is a number — is what determines whether it may carry a default:

- `exposure` — a public hostname, a bind address, a published port, a URL a
  client will trust. **Never defaulted**, anywhere: code, images, docs, or
  examples.
- `allocation` — a path or a slot on a host the operator owns. **Always
  defaulted**, so a deployment does not fail merely because the example did
  not know its host's filesystem layout.
- `behaviour` — tuning that decides neither of the above; unconstrained.

`tests/test_config.py` asserts the first two rules against the live schema,
so the policy is enforced before there is an exposure field to get wrong.
"""

from __future__ import annotations

import json
import os
import tomllib
from collections.abc import Mapping
from dataclasses import dataclass
from pathlib import Path
from types import UnionType
from typing import Any, Literal, Union, get_args, get_origin

from pydantic import Field
from pydantic.fields import FieldInfo
from pydantic_settings import (
    BaseSettings,
    PydanticBaseSettingsSource,
    SettingsConfigDict,
)

ENV_PREFIX = "VOGT_"
CONFIG_FILE_ENV = "VOGT_CONFIG_FILE"

DefaultPolicy = Literal["exposure", "allocation", "behaviour"]
LogLevel = Literal["debug", "info", "warning", "error"]

DECLARED_DB_NAME = "declared.sqlite3"
OBSERVED_DB_NAME = "observed.sqlite3"
BACKUPS_DIR_NAME = "backups"
IMPORT_DIR_NAME = "repos"


def default_data_dir() -> Path:
    """Where an instance lives when the operator has not said otherwise.

    An allocation value: it names a directory on a host the operator owns,
    and gating it would only produce a tool that refuses to start.
    """
    xdg = os.environ.get("XDG_DATA_HOME")
    base = Path(xdg) if xdg else Path.home() / ".local" / "share"
    return base / "vogt"


class _TomlFileSource(PydanticBaseSettingsSource):
    """Reads the optional TOML file named by ``VOGT_CONFIG_FILE``."""

    def _values(self) -> dict[str, Any]:
        raw = os.environ.get(CONFIG_FILE_ENV)
        if not raw:
            return {}
        path = Path(raw).expanduser()
        if not path.is_file():
            return {}
        with path.open("rb") as handle:
            loaded: dict[str, Any] = tomllib.load(handle)
        return loaded

    def get_field_value(
        self, field: FieldInfo, field_name: str
    ) -> tuple[Any, str, bool]:
        return self._values().get(field_name), field_name, False

    def __call__(self) -> dict[str, Any]:
        known = set(VogtConfig.model_fields)
        return {k: v for k, v in self._values().items() if k in known}


class VogtConfig(BaseSettings):
    """The configuration of one Vogt instance."""

    model_config = SettingsConfigDict(
        env_prefix=ENV_PREFIX,
        extra="forbid",
        frozen=True,
    )

    data_dir: Path = Field(
        default_factory=default_data_dir,
        description=(
            "Directory holding declared.sqlite3, observed.sqlite3 and backups. "
            "One instance per directory."
        ),
        json_schema_extra={"default_policy": "allocation"},
    )
    import_root: Path | None = Field(
        default=None,
        description=(
            "Directory imported repositories are cloned into, one per project "
            "slug (FR-P6). An allocation value, so it has a default rather "
            "than a gate: unset means `<data_dir>/repos`, which keeps the "
            "clone with the instance that registered it. Deployments that "
            "observe an estate on a mounted host directory point this at that "
            "estate instead, so an imported project lands where the rest of "
            "the work already lives."
        ),
        json_schema_extra={"default_policy": "allocation"},
    )
    public_url: str | None = Field(
        default=None,
        description=(
            "The URL clients reach this instance at, e.g. "
            "`https://vogt.example.com`. An exposure value, so it "
            "has no default and is never guessed: the process binds "
            "`0.0.0.0:8000` inside a container and is published somewhere "
            "else entirely, so the server cannot know its own address — only "
            "the operator does. Unset means `connect` reports that nobody has "
            "said, which is a different answer from reporting a URL that does "
            "not work (FR-A8)."
        ),
        json_schema_extra={"default_policy": "exposure"},
    )
    fronted: bool = Field(
        default=False,
        description=(
            "Whether this instance runs behind an optional front door that "
            "publishes it at a different address and mount points. When true, "
            "`connect` and "
            "`/connection-info` render against the identity the door states "
            "per request (`X-Vogt-Public-Url`, `X-Vogt-Api-Path`, "
            "`X-Vogt-Mcp-Path`), because the door is the only thing that "
            "knows where clients arrive (FR-A8, FR-A9). Off by default and "
            "never inferred: an instance that has not been told it is fronted "
            "ignores those headers entirely, so nobody who can reach it can "
            "make `connect` render a client configuration — a document meant "
            "to be pasted next to a token — against an address they chose."
        ),
        json_schema_extra={"default_policy": "behaviour"},
    )
    log_level: LogLevel = Field(
        default="info",
        description=(
            "Verbosity of Vogt's own diagnostics — the `vogt.*` logger "
            "namespace, not its dependencies'."
        ),
        json_schema_extra={"default_policy": "behaviour"},
    )
    log_format: Literal["text", "json"] = Field(
        default="text",
        description=(
            "How each line is rendered. `text` is for a person reading "
            "`docker logs`; `json` is one object per line for a log that is "
            "queried rather than read (Loki). The fields are the same either "
            "way, so a query written against one describes the other."
        ),
        json_schema_extra={"default_policy": "behaviour"},
    )
    log_requests: bool = Field(
        default=True,
        description=(
            "Whether every served request produces an access line carrying "
            "its duration (NFR-OB1). On by default: the 2026-08-19 incident "
            "was diagnosed by counting paths across three thousand stock "
            "uvicorn lines that had no timing on them at all, and a single "
            "slow endpoint would have left no trace. Turning this off still "
            "leaves correlation ids honoured and echoed (NFR-OB3)."
        ),
        json_schema_extra={"default_policy": "behaviour"},
    )
    log_slow_request_ms: int = Field(
        default=1000,
        ge=0,
        description=(
            "A request whose response takes longer than this to *start* is "
            "logged at WARNING rather than INFO (NFR-OB2). Judged on time to "
            "first byte, so a long-lived `/mcp` stream is not reported as a "
            "pathological request every time it ends."
        ),
        json_schema_extra={"default_policy": "behaviour"},
    )
    log_quiet_paths: tuple[str, ...] = Field(
        default=("/health/live", "/health/ready", "/version"),
        description=(
            "Paths whose access lines drop to DEBUG (NFR-OB4). Probes, by "
            "default: an orchestrator calls them every few seconds forever, "
            "and a log that is 100% `/healthz` with no application output in "
            "it is not a log. Suppressed and not dropped — a probe that "
            "crosses `log_slow_request_ms` still warns, which is exactly what "
            "`/health/ready` did for 25 seconds on 2026-08-19."
        ),
        json_schema_extra={"default_policy": "behaviour"},
    )
    contract_required_files: tuple[str, ...] = Field(
        default=("AGENTS.md", "README.md", "LICENSE"),
        description=(
            "Files a compliant project must contain (FR-G1). The contract is "
            "a value you read, never a barrier you pass: changing this "
            "changes what `contract check` reports and gates nothing "
            "(FR-G13)."
        ),
        json_schema_extra={"default_policy": "behaviour"},
    )
    contract_required_dirs: tuple[str, ...] = Field(
        default=("docs", "design", "src"),
        description="Directories a compliant project must contain (FR-G1).",
        json_schema_extra={"default_policy": "behaviour"},
    )
    contract_required_meta: tuple[str, ...] = Field(
        default=("name", "lifecycle_state", "owner"),
        description=(
            "Metadata keys a compliant project must declare (FR-G1). Read "
            "from the project's own manifest, not from Vogt's registration."
        ),
        json_schema_extra={"default_policy": "behaviour"},
    )
    contract_version: str = Field(
        default="v1",
        description=(
            "Names which contract a recorded compliance status was evaluated "
            "against (FR-G1, FR-G3). If the rules above differ from the "
            "built-in defaults and this is left at its own default, Vogt "
            "appends a short digest of the rules — a status must never claim "
            "to be the stock `v1` when it is not, and an operator who edits "
            "the rules should not have to remember to rename them."
        ),
        json_schema_extra={"default_policy": "behaviour"},
    )
    marker_promotion_patterns: tuple[str, ...] = Field(
        default=("TODO(vogt)", "FIXME(vogt)"),
        description=(
            "Source markers containing one of these enter backlog and bug "
            "views (FR-W11). Every other marker is still observed, still "
            "queryable and still counted; it just does not claim to be work. "
            "Widening this is how you drown the ranked view."
        ),
        json_schema_extra={"default_policy": "behaviour"},
    )
    marker_file_extensions: tuple[str, ...] = Field(
        default=(
            ".py",
            ".rs",
            ".ts",
            ".tsx",
            ".js",
            ".jsx",
            ".go",
            ".java",
            ".rb",
            ".sh",
            ".sql",
            ".toml",
            ".yaml",
            ".yml",
            ".md",
        ),
        description=(
            "File types the marker collector reads. Configuration rather "
            "than a hard-coded list, because which extensions hold source "
            "is an estate's business, not Vogt's (FR-W11)."
        ),
        json_schema_extra={"default_policy": "behaviour"},
    )
    retention_days: int = Field(
        default=180,
        ge=1,
        description=(
            "How long observation *history* is kept (NFR-I5). The newest "
            "observation per subject is kept indefinitely regardless, and so "
            "is anything a drift proposal references."
        ),
        json_schema_extra={"default_policy": "behaviour"},
    )
    github_token_file: Path | None = Field(
        default=None,
        description=(
            "Path to a file containing a GitHub token. Its absence is what "
            "switches the optional forge adapter off, so there is no default: "
            "not configured is the ordinary case, and it means forge subjects "
            "are 'not collected' rather than absent. A file rather than an "
            "environment variable or an argument, so the token never appears "
            "in a process listing (FR-S7)."
        ),
        json_schema_extra={"default_policy": "behaviour"},
    )
    forge_token_files: dict[str, Path] = Field(
        default_factory=dict,
        description=(
            "Per-host forge token files (D8), mapping a forge host to a file "
            "holding a token for it — a TOML table `[forge_token_files]` with, "
            'e.g., `"github.com" = "/run/secrets/github_token"`. This is the '
            "general form of `github_token_file`, which stays as the alias for "
            "github.com; a host set here wins over the alias. A host absent "
            "from this map has no provider registered, which is what keeps its "
            "subjects 'not collected' rather than reported as absent — the same "
            "honesty rule the single-token field has always followed. Files "
            "rather than environment variables or arguments, so a token never "
            "appears in a process listing (FR-S7)."
        ),
        json_schema_extra={"default_policy": "behaviour"},
    )
    engine_url: str | None = Field(
        default=None,
        description=(
            "Where the session engine listens, e.g. `http://127.0.0.1:8910`. "
            "The engine is the other half of the merged product: it owns the "
            "PTYs a work item's session runs in (FR-E1). Unset means the "
            "`session.*` operations report that no engine is configured — "
            "absence of the engine costs sessions and nothing else, which is "
            "FR-E9 read from this side. An exposure value, so it is never "
            "guessed: co-located today, its address is still the operator's "
            "to state."
        ),
        json_schema_extra={"default_policy": "exposure"},
    )
    session_scratch_project: str | None = Field(
        default=None,
        description=(
            "The project slug a session with no work item and no project "
            "resolves to (FR-T11). It exists for the spoken request that has "
            "no subject — 'research the best risotto in Wollongong' — which "
            "still needs a registered working tree to open in. Unset means "
            "such a request is refused by name rather than opened somewhere "
            "guessed, which is FR-E3's whole point: the working directory "
            "comes from the registry, and a scratch project is a registered "
            "project like any other."
        ),
        json_schema_extra={"default_policy": "behaviour"},
    )
    engine_state_dir: Path | None = Field(
        default=None,
        description=(
            "The session engine's state directory, when this process can read "
            "it — the merged deployment runs both halves in one container, so "
            "it usually can. `backup` copies it and `restore` puts it back, "
            "because half a restore is the failure NFR-I6 exists to prevent: "
            "the work items come back and the terminals' history, push "
            "subscriptions and agent tasks do not. Its absence is what makes "
            "a backup a core-only backup, so there is no default — the same "
            "reason `github_token_file` has none. Unset, the manifest says "
            "'not configured' rather than quietly covering less than it "
            "appears to."
        ),
        json_schema_extra={"default_policy": "behaviour"},
    )
    engine_token_file: Path | None = Field(
        default=None,
        description=(
            "Path to a file containing the engine token Vogt calls it with. "
            "The token needs only the engine's `sessions` capability: Vogt "
            "starts and stops terminals, and has no business writing that "
            "pod's files. A file rather than an environment variable, for the "
            "same reason as `github_token_file` — a token in the environment "
            "is a token in every `docker inspect` (FR-S7)."
        ),
        json_schema_extra={"default_policy": "behaviour"},
    )
    sqlite_synchronous: Literal["off", "normal", "full", "extra"] = Field(
        default="normal",
        description=(
            "How hard SQLite works to survive a power cut. `normal` is the "
            "standard pairing for WAL and the default here; `full` fsyncs the "
            "write-ahead log on every commit, which on a contended disk costs "
            "tens of milliseconds per write. Under `normal` a power loss or "
            "OS crash can lose the last few committed transactions — the "
            "database is never corrupted, and an application crash loses "
            "nothing. Set `full` if you would rather pay that cost per write."
        ),
        json_schema_extra={"default_policy": "behaviour"},
    )
    sweep_interval_seconds: int = Field(
        default=900,
        ge=0,
        description=(
            "How often `serve` runs collectors in the background (FR-L3). "
            "Zero disables the schedule, leaving sweeps on-demand only. A "
            "default rather than a required value because an instance that "
            "never looks is the failure this product exists to prevent: "
            "stale evidence and no evidence are indistinguishable from the "
            "outside, so the safe default is to keep looking."
        ),
        json_schema_extra={"default_policy": "behaviour"},
    )
    verify_horizon_hours: int = Field(
        default=24,
        ge=1,
        description=(
            "How recently a subject must have been observed for a linked "
            "declared entity to count as `verified` rather than `stale` "
            "(FR-R4). Trust is computed from this, never hand-set."
        ),
        json_schema_extra={"default_policy": "behaviour"},
    )

    @classmethod
    def settings_customise_sources(
        cls,
        settings_cls: type[BaseSettings],
        init_settings: PydanticBaseSettingsSource,
        env_settings: PydanticBaseSettingsSource,
        dotenv_settings: PydanticBaseSettingsSource,
        file_secret_settings: PydanticBaseSettingsSource,
    ) -> tuple[PydanticBaseSettingsSource, ...]:
        return (
            init_settings,
            env_settings,
            _TomlFileSource(settings_cls),
            file_secret_settings,
        )

    @property
    def declared_db_path(self) -> Path:
        return self.resolved_data_dir / DECLARED_DB_NAME

    @property
    def observed_db_path(self) -> Path:
        return self.resolved_data_dir / OBSERVED_DB_NAME

    @property
    def backups_dir(self) -> Path:
        return self.resolved_data_dir / BACKUPS_DIR_NAME

    @property
    def resolved_data_dir(self) -> Path:
        return self.data_dir.expanduser()

    @property
    def resolved_import_root(self) -> Path:
        """Where `project.import` clones to (FR-P6).

        Derived from `data_dir` when unset, which is why the field itself is
        optional while the *value* always exists: a default factory cannot
        see another field, and an allocation value must never be a gate
        (NFR-D2).
        """
        if self.import_root is not None:
            return self.import_root.expanduser()
        return self.resolved_data_dir / IMPORT_DIR_NAME


def load_config(**overrides: Any) -> VogtConfig:
    """Build the configuration, applying the documented precedence."""
    return VogtConfig(**overrides)


@dataclass(frozen=True)
class FieldDoc:
    """One documented configuration field, as the generators see it."""

    name: str
    env_var: str
    type_label: str
    default_label: str
    policy: DefaultPolicy
    description: str


_SCALAR_NAMES = {str: "string", int: "integer", float: "number", bool: "boolean"}


def _label_for(annotation: Any) -> str:
    """Name one type in prose, never by its module path.

    `str(annotation)` is not usable here: it renders `Path` as
    `pathlib.Path` on Python 3.11 and `pathlib._local.Path` on 3.13, so the
    generated documentation differed by interpreter and the drift check
    failed on one version of the CI matrix and not the other. Documentation
    should not leak where the standard library happens to keep a class.
    """
    if annotation is Path:
        return "path"
    if annotation in _SCALAR_NAMES:
        return _SCALAR_NAMES[annotation]

    origin = get_origin(annotation)
    args = get_args(annotation)
    if origin is Literal:
        return "one of " + ", ".join(f"`{arg}`" for arg in args)
    if origin in (tuple, list, set, frozenset):
        inner = args[0] if args else str
        return f"list of {_SCALAR_NAMES.get(inner, 'values')}s"
    if origin is dict:
        key, value = (args[0], args[1]) if len(args) == 2 else (str, str)
        return f"map of {_label_for(key)} to {_label_for(value)}"
    if origin in (Union, UnionType):
        present = [arg for arg in args if arg is not type(None)]
        rendered = " or ".join(_label_for(arg) for arg in present)
        return f"{rendered}, optional" if len(present) < len(args) else rendered
    return getattr(annotation, "__name__", "value")


def _type_label(field: FieldInfo) -> str:
    """Describe a field's type in prose that survives a Markdown table.

    Deliberately not `repr(annotation)`: a `Literal` renders as
    `'debug' | 'info' | ...`, and those pipes silently split the generated
    table into extra columns. The generated docs are the only description of
    these settings there is, so they have to be right.
    """
    return _label_for(field.annotation)


def _default_label(name: str, field: FieldInfo) -> str:
    if field.default_factory is not None:
        if name == "data_dir":
            return "`$XDG_DATA_HOME/vogt`, else `~/.local/share/vogt`"
        if name == "forge_token_files":
            return "*(empty)*"
        return "computed"  # pragma: no cover - no other factory fields yet
    if name == "import_root":
        # Derived from another field, so it cannot be a factory — but it is
        # an allocation value and documenting it as "no default" would be a
        # lie the policy test would rightly fail on.
        return "`<data_dir>/repos`"
    if field.default is None:
        return "*(no default — must be set)*"
    if isinstance(field.default, tuple):
        if not field.default:
            return "*(empty)*"
        return ", ".join(f"`{entry}`" for entry in field.default)
    return f"`{field.default}`"


def _policy_of(field: FieldInfo) -> DefaultPolicy:
    extra = field.json_schema_extra
    if isinstance(extra, dict):
        value = extra.get("default_policy")
        if value == "exposure":
            return "exposure"
        if value == "allocation":
            return "allocation"
        if value == "behaviour":
            return "behaviour"
    msg = "every config field must declare a default_policy (NFR-D2)"
    raise ValueError(msg)


def describe_fields() -> list[FieldDoc]:
    """Introspect the schema — the input to every generated artefact."""
    docs: list[FieldDoc] = []
    for name, field in VogtConfig.model_fields.items():
        docs.append(
            FieldDoc(
                name=name,
                env_var=f"{ENV_PREFIX}{name.upper()}",
                type_label=_type_label(field),
                default_label=_default_label(name, field),
                policy=_policy_of(field),
                description=field.description or "",
            )
        )
    return docs


_GENERATED_BANNER = (
    "<!-- Generated by scripts/gen_config_docs.py from src/vogt/config.py. "
    "Do not edit by hand; CI fails on drift (NFR-Q4). -->"
)


def render_config_reference() -> str:
    """Render `docs/CONFIG.md` from the schema."""
    fields = describe_fields()
    lines = [
        _GENERATED_BANNER,
        "",
        "# Vogt — Configuration Reference",
        "",
        "Every value below comes from `src/vogt/config.py`, which is the",
        "single source of truth (NFR-Q4). Precedence, highest first:",
        "explicit arguments, `VOGT_*` environment variables, the TOML file",
        f"named by `{CONFIG_FILE_ENV}`, then the defaults shown here.",
        "",
        "## Settings",
        "",
        "| Setting | Env var | Type | Default | Default policy |",
        "|---|---|---|---|---|",
    ]
    for field in fields:
        lines.append(
            f"| `{field.name}` | `{field.env_var}` | {field.type_label} "
            f"| {field.default_label} | {field.policy} |"
        )
    lines += ["", "## What each setting decides", ""]
    for field in fields:
        lines += [f"### `{field.name}`", "", field.description, ""]
    lines += [
        "## Default policy (NFR-D2, revised r4)",
        "",
        "- **exposure** — hostnames, bind addresses, published ports, URLs a",
        "  client will trust. These never carry a default, in code, images,",
        "  docs or examples.",
        "- **allocation** — paths and slots on a host the operator owns.",
        "  These always carry a default: gating them produces broken deploys,",
        "  not safety (`DEPLOYMENT.md` §4.1).",
        "- **behaviour** — tuning that decides neither; unconstrained.",
        "",
    ]
    return "\n".join(lines)


def render_example_config() -> str:
    """Render `config.example.toml` from the schema."""
    fields = describe_fields()
    lines = [
        "# Generated by scripts/gen_config_docs.py from src/vogt/config.py.",
        "# Do not edit by hand; CI fails on drift (NFR-Q4).",
        "#",
        "# Every setting is commented out and shows its default. Point Vogt at",
        f"# a copy of this file with {CONFIG_FILE_ENV}=/path/to/vogt.toml.",
        "",
    ]
    for field in fields:
        lines += [f"# {field.description}", f"# default policy: {field.policy}"]
        example = _example_value(field)
        lines += [f"# {field.name} = {example}", ""]
    return "\n".join(lines)


def _example_value(field: FieldDoc) -> str:
    """Render a field's default the way a TOML file would spell it."""
    default = VogtConfig.model_fields[field.name].default
    if field.name == "data_dir":
        return '"/var/lib/vogt"'
    if field.name == "import_root":
        return '"/var/lib/vogt/repos"'
    if field.name == "forge_token_files":
        # An inline table shows the shape an operator gets wrong — the host is
        # the key, the token *file* the value — where an empty `{}` would not.
        return '{ "github.com" = "/run/secrets/github_token" }'
    if field.name == "public_url":
        # Shown as an example rather than `null`, because an exposure value
        # with no default still has a *shape*, and the shape is the part an
        # operator gets wrong.
        return '"https://host.tailnet.ts.net:18094"'
    if isinstance(default, tuple):
        rendered = ", ".join(json.dumps(entry) for entry in default)
        return f"[{rendered}]"
    if isinstance(default, bool):
        return "true" if default else "false"
    if isinstance(default, int):
        return str(default)
    return json.dumps(default)


def config_artifacts(root: Path) -> Mapping[Path, str]:
    """The generated files, keyed by their committed location."""
    return {
        root / "docs" / "CONFIG.md": render_config_reference(),
        root / "config.example.toml": render_example_config(),
    }
