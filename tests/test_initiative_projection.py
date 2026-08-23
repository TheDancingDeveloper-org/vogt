"""The pure half of the initiative projection (#286): rendering and splicing.

No forge in the loop — these pin the string shapes the write path depends on:
the task list (`- [ ] #n`, done → checked), the managed markers, the splice
that preserves human text outside the region, adopt-by-marker, and the
checkbox parse that later turns an upstream tick into drift.
"""

from __future__ import annotations

from vogt.core.initiative_projection import (
    EMPTY_TASK_LIST,
    MANAGED_END,
    MANAGED_START,
    TaskLine,
    body_has_marker,
    marker_for,
    parse_checkbox_states,
    render_managed_region,
    render_task_list,
    splice_managed_region,
)


def test_task_list_renders_open_as_unchecked_and_terminal_as_checked() -> None:
    lines = [
        TaskLine.from_state(number=1, title="Wire the seam", state="in_progress"),
        TaskLine.from_state(number=2, title="Land the tests", state="done"),
        TaskLine.from_state(number=3, title="Abandoned idea", state="wont_do"),
    ]
    rendered = render_task_list(lines)
    assert "- [ ] #1 Wire the seam" in rendered
    assert "- [x] #2 Land the tests" in rendered
    # `wont_do` is terminal too — a task list mirrors the workflow, not just done.
    assert "- [x] #3 Abandoned idea" in rendered


def test_an_empty_task_list_reads_as_empty_not_as_a_bug() -> None:
    assert render_task_list([]) == EMPTY_TASK_LIST


def test_the_managed_region_carries_the_marker_and_the_body() -> None:
    region = render_managed_region(
        slug="platform",
        body="The platform epic.",
        tasks=[TaskLine(number=7, title="Do it", checked=False)],
    )
    assert region.startswith(MANAGED_START)
    assert region.rstrip().endswith(MANAGED_END)
    assert marker_for("platform") in region
    assert "The platform epic." in region
    assert "- [ ] #7 Do it" in region
    assert body_has_marker(region, "platform")
    assert not body_has_marker(region, "other-epic")


def test_a_single_repo_region_has_no_cross_reference_block() -> None:
    region = render_managed_region(slug="s", body="", tasks=[])
    assert "Tracked across" not in region


def test_cross_references_are_rendered_when_siblings_are_present() -> None:
    region = render_managed_region(
        slug="s",
        body="",
        tasks=[],
        siblings=[("other", "https://github.com/acme/other/issues/4")],
    )
    assert "Tracked across other repositories" in region
    assert "- other: https://github.com/acme/other/issues/4" in region


def test_splice_preserves_text_outside_the_markers() -> None:
    existing = (
        "# Human title\n\nSome notes a person wrote.\n\n"
        f"{MANAGED_START}\nold managed body\n{MANAGED_END}\n\n"
        "A human footer below."
    )
    region = render_managed_region(
        slug="s", body="fresh", tasks=[TaskLine(number=1, title="t", checked=False)]
    )
    out = splice_managed_region(existing, region)
    # Human prose above and below the region survives verbatim.
    assert "# Human title" in out
    assert "Some notes a person wrote." in out
    assert "A human footer below." in out
    # The old managed content is gone; the new one is in.
    assert "old managed body" not in out
    assert "- [ ] #1 t" in out
    # Idempotent: splicing the same region again changes nothing.
    assert splice_managed_region(out, region) == out


def test_splice_appends_when_the_body_has_no_region_yet() -> None:
    region = render_managed_region(slug="s", body="", tasks=[])
    out = splice_managed_region("A pre-existing human issue body.", region)
    assert out.startswith("A pre-existing human issue body.")
    assert MANAGED_START in out
    # And a None body yields just the region.
    assert splice_managed_region(None, region) == region


def test_parse_reads_checkbox_states_from_the_managed_region_only() -> None:
    body = (
        "- [x] #99 a human checkbox above the region\n\n"
        f"{MANAGED_START}\n{marker_for('s')}\n\n"
        "- [ ] #1 open member\n- [x] #2 done member\n"
        f"{MANAGED_END}\n"
    )
    states = parse_checkbox_states(body)
    # #99 is outside the region and must not be read as a member.
    assert states == {1: False, 2: True}


def test_parse_of_a_body_without_a_region_is_empty() -> None:
    assert parse_checkbox_states("no region here") == {}
