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

REQUIREMENTS = Path(__file__).resolve().parents[1] / "docs" / "REQUIREMENTS.md"

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
