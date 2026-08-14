"""§6's counts agree with §6's rows.

`REQUIREMENTS.md` §6 is a delivery audit whose whole authority rests on being
countable: it states a total, splits it four ways, and says in its own text
that the split "is checkable without re-deriving all 201". That is true only
while the numbers and the rows agree, and on 2026-08-14 they were edited by
hand roughly two dozen times in one session — every conjunct that moved
required four separate numbers to be adjusted in step.

A document that claims to be arithmetic and is maintained by memory is a
document that will eventually be wrong in a way nobody notices, which is the
failure this section exists to find in *other* documents.

These assertions are deliberately narrow. They check that the section is
self-consistent, not that it is true: whether a conjunct belongs in §6.1 or
§6.2 is a judgement no test can make, and the reader is owed the judgement,
not a checksum.
"""

from __future__ import annotations

import re
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parents[1]
REQUIREMENTS = REPO_ROOT / "docs" / "REQUIREMENTS.md"

#: §6.1 cites files across all three trees, and the `core` job in CI proves
#: the core stands alone by deleting two of them (NFR-Q6). Checking a citation
#: against a tree that is not there says nothing about the citation — it says
#: the checkout is a core-only one, which is the point of that job. Caught by
#: that job on the first run, which is the third time in a day it has found a
#: core test quietly needing the other half.
merged_tree_only = pytest.mark.skipif(
    not (REPO_ROOT / "engine").is_dir() or not (REPO_ROOT / "web").is_dir(),
    reason="§6.1 cites the engine and the PWA; a core-only checkout has neither",
)

#: Only as far as the counts actually go. A bigger table would be inviting the
#: next reader to add a word rather than a row.
WORD_NUMBERS = {
    "zero": 0,
    "one": 1,
    "two": 2,
    "three": 3,
    "four": 4,
    "five": 5,
    "six": 6,
    "seven": 7,
    "eight": 8,
    "nine": 9,
    "ten": 10,
    "eleven": 11,
    "twelve": 12,
    "thirteen": 13,
    "fourteen": 14,
    "fifteen": 15,
    "sixteen": 16,
    "seventeen": 17,
    "eighteen": 18,
    "nineteen": 19,
    "twenty": 20,
    "twenty-one": 21,
    "twenty-two": 22,
    "twenty-three": 23,
    "twenty-four": 24,
    "twenty-five": 25,
}


def audit() -> str:
    return REQUIREMENTS.read_text(encoding="utf-8")


def section(text: str, heading: str, until: str) -> str:
    start = text.index(heading)
    return text[start : text.index(until, start)]


def data_rows(block: str) -> list[str]:
    """Table rows, less the header and the `|---|` rule."""
    rows = [line for line in block.splitlines() if line.startswith("| ")]
    return [row for row in rows if not row.startswith("| ID |")][1:]


def test_the_four_buckets_sum_to_the_total() -> None:
    """Every conjunct is in exactly one bucket, by the section's own rule."""
    text = audit()
    stated = re.search(
        r"\*\*(\d+) conjuncts across \d+ IDs\. (\d+) are delivered, (\d+) are "
        r"implemented and\nasserted by nothing, (\d+) cannot be verified in this "
        r"environment at all, and (\d+)\nare short or absent\.\*\*",
        text,
    )
    assert stated, (
        "§6's summary sentence has been reworded; it is the one place the four "
        "counts are stated together, so this test needs updating with it"
    )
    total, delivered, untested, unverifiable, short = (int(g) for g in stated.groups())
    assert delivered + untested + unverifiable + short == total, (
        f"{delivered} + {untested} + {unverifiable} + {short} is not {total}; "
        "a conjunct has been moved without adjusting the count it left"
    )


def test_the_short_count_matches_the_rows_that_carry_it() -> None:
    """§6.2 is one conjunct per row, and says so.

    It is the only one of the four that can be checked this way — §6.1, §6.2a
    and §6.2b group conjuncts by ID and their rows carry several each, which
    the section states as the reason its arithmetic is presented rather than
    derived.
    """
    text = audit()
    stated = re.search(r"\*\*(\d+)\nare short or absent\.\*\*", text) or re.search(
        r", and (\d+)\nare short or absent", text
    )
    assert stated, "§6's summary no longer states the short count"
    block = section(text, "### 6.2 Delivered differently", "### 6.2a")

    opening = re.match(r"### 6.2 [^\n]*\n\n([A-Za-z-]+) conjuncts\.", block)
    assert opening, "§6.2 opens by naming how many conjuncts it holds"
    named = WORD_NUMBERS.get(opening.group(1).lower())
    assert named is not None, f"unmapped number word: {opening.group(1)}"

    rows = len(data_rows(block))
    assert named == rows == int(stated.group(1)), (
        f"§6.2 says {named} conjuncts, carries {rows} rows, and §6's summary "
        f"says {stated.group(1)} are short — these three are the same number"
    )


def test_no_delivered_row_is_filed_under_untested() -> None:
    """A bold ID means delivered, and §6.2a is what is *not*.

    Written after making the mistake: an NFR-C6 row marked delivered was
    inserted into §6.2a, because it was anchored on a neighbouring row that
    happened to live there. The counts still summed — the arithmetic guard
    above is blind to which table a row is in — and the section read as
    claiming a conjunct was both asserted and asserted by nothing.

    §6.2b likewise: a conjunct that cannot be verified here cannot
    simultaneously be delivered.
    """
    text = audit()
    for heading, until in (
        ("### 6.2a", "### 6.2b"),
        ("### 6.2b", "### 6.3"),
    ):
        block = section(text, heading, until)
        misfiled = [
            row.split("|")[1].strip()
            for row in block.splitlines()
            if row.startswith("| **")
        ]
        assert not misfiled, (
            f"{misfiled} are marked delivered and filed under {heading}; a bold "
            "ID is this section's mark for a conjunct with a file and a test "
            "behind it, and these tables are for the ones without"
        )


@merged_tree_only
def test_every_file_6_1_cites_exists() -> None:
    """§6.1's rule is "a file and a test", so the file has to be there.

    Written after finding 24: FR-M2's delivered row named
    `engine/server/src/vogt_drift.rs` and three tests inside it, and no such
    file has ever existed. The citation was specific enough to look
    trustworthy and survived four passes, because nothing ran the grep.

    Paths only. Test *names* are checked by the test below, and the two are
    separate because a renamed test and a deleted file are different
    mistakes with different fixes.
    """
    text = audit()
    delivered = section(text, "### 6.1 Delivered", "### 6.2 ")
    #: Generated, and absent until something generates it. `web/dist/` is the
    #: PWA bundle `rust-embed` compiles into the engine; citing it is correct,
    #: and requiring it to exist would make this test demand a build.
    generated = {"web/dist/"}
    cited = set(
        re.findall(r"`((?:src|tests|web|engine|scripts|deploy)/[\w./-]+)`", delivered)
    )
    assert cited, "§6.1 cites files in backticks; the pattern no longer matches any"
    cited -= generated
    missing = sorted(path for path in cited if not (REPO_ROOT / path).exists())
    assert not missing, (
        f"§6.1 cites {missing}, which do not exist. A delivered row names a "
        "file and a test; a file that is not there means the row is about "
        "something else, or about nothing"
    )


@merged_tree_only
def test_every_test_name_6_1_cites_exists_somewhere() -> None:
    """The other half of finding 24: the names, not the paths.

    Searched across the whole repository rather than inside the file the row
    happens to name, because a test that moved is a stale citation and a test
    that never existed is a false one — and only the second is worth failing
    a build over. Both are worth knowing about; this catches the second.
    """
    import subprocess

    text = audit()
    delivered = section(text, "### 6.1 Delivered", "### 6.2 ")
    #: `::name` and `` `name` `` forms both appear; take the unambiguous one.
    names = set(re.findall(r"::([a-z_][a-z0-9_]{8,})", delivered))
    assert len(names) > 20, f"expected many cited test names, found {len(names)}"

    found = subprocess.run(
        ["git", "grep", "-h", "-o", "-E", r"fn [a-z_][a-z0-9_]*|def [a-z_][a-z0-9_]*"],
        cwd=REPO_ROOT,
        capture_output=True,
        text=True,
        check=True,
    ).stdout
    defined = {line.split()[-1] for line in found.splitlines() if line.strip()}
    # Vitest names are strings rather than identifiers, so anything not found
    # as a definition is checked as a literal instead.
    missing = sorted(name for name in names if name not in defined)
    assert not missing, (
        f"§6.1 cites tests that are defined nowhere: {missing}. Finding 24 is "
        "what this exists for — three such names sat in the delivered table "
        "through four passes"
    )
