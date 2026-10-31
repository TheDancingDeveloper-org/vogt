"""Project `mcp_extra` drift: names no host config registers (WI-2746)."""

from __future__ import annotations

import json
from pathlib import Path

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
