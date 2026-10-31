"""Project `mcp_extra` drift: names no host config registers (WI-2746)."""

from __future__ import annotations

import json
from pathlib import Path

import pytest
from pydantic import ValidationError

from vogt.application.models import UpdateProjectParams
from vogt.application.services.projects import unregistered_mcp


def test_unregistered_extras_are_the_ones_no_config_names(tmp_path: Path) -> None:
    (tmp_path / ".claude.json").write_text(
        json.dumps({"mcpServers": {"vogt": {}, "komodo": {}}})
    )
    (tmp_path / ".codex").mkdir()
    (tmp_path / ".codex" / "config.toml").write_text(
        "[mcp_servers.grafana]\ncommand = 'g'\n"
    )
    assert unregistered_mcp(["komodo", "grafana", "ghost"], home=tmp_path) == ["ghost"]


def test_no_configs_register_nothing(tmp_path: Path) -> None:
    assert unregistered_mcp(["komodo"], home=tmp_path) == ["komodo"]
    assert unregistered_mcp([], home=tmp_path) == []


def test_an_accounts_config_dir_registers_too(tmp_path: Path) -> None:
    home = tmp_path / "home"
    home.mkdir()
    account = tmp_path / "acct2"
    account.mkdir()
    (account / ".claude.json").write_text(json.dumps({"mcpServers": {"komodo": {}}}))
    assert unregistered_mcp(["komodo"], home=home) == ["komodo"]
    assert unregistered_mcp(["komodo"], home=home, claude_dirs=[account]) == []


@pytest.mark.parametrize(
    "names",
    [["bad name"], ["a.b"], ["x" * 65], [f"s{i}" for i in range(17)]],
)
def test_mcp_extra_names_are_validated(names: list[str]) -> None:
    with pytest.raises(ValidationError):
        UpdateProjectParams(slug="p", reason="r", mcp_extra=names)


def test_good_mcp_extra_names_pass() -> None:
    params = UpdateProjectParams(slug="p", reason="r", mcp_extra=["komodo", "g_1-x"])
    assert params.mcp_extra == ["komodo", "g_1-x"]
