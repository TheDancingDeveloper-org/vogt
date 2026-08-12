"""Configuration — the single source of truth (NFR-Q4).

`docs/CONFIG.md` and `config.example.toml` are *generated* from the schema
below by `scripts/gen_config_docs.py`, and CI fails when the committed files
drift from it. Cadastre's `:18081` incident began as a default documented in
one place and implemented in another; here there is only one place.

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
  defaulted**, because `${X:?}`-gating allocation values is what cost
  cadastre every deploy after `cadastre#42`.
- `behaviour` — tuning that decides neither of the above; unconstrained.

`tests/test_config.py` asserts the first two rules against the live schema,
so the policy is enforced before there is an exposure field to get wrong.
"""

from __future__ import annotations

import os
import tomllib
from collections.abc import Mapping
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Literal

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
    log_level: LogLevel = Field(
        default="info",
        description="Verbosity of Vogt's own diagnostics.",
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


def _type_label(field: FieldInfo) -> str:
    annotation = field.annotation
    if annotation is Path:
        return "path"
    origin = getattr(annotation, "__args__", None)
    if origin:
        return " | ".join(repr(arg) for arg in origin)
    return getattr(annotation, "__name__", str(annotation))


def _default_label(name: str, field: FieldInfo) -> str:
    if field.default_factory is not None:
        if name == "data_dir":
            return "`$XDG_DATA_HOME/vogt`, else `~/.local/share/vogt`"
        return "computed"  # pragma: no cover - no other factory fields yet
    if field.default is None:
        return "*(no default — must be set)*"
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
    if field.name == "data_dir":
        return '"/var/lib/vogt"'
    if field.name == "log_level":
        return '"info"'
    return '""'  # pragma: no cover - no other fields yet


def config_artifacts(root: Path) -> Mapping[Path, str]:
    """The generated files, keyed by their committed location."""
    return {
        root / "docs" / "CONFIG.md": render_config_reference(),
        root / "config.example.toml": render_example_config(),
    }
