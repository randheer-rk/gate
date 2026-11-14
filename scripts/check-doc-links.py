"""Check every static link from markdown prose into code.

A `<code:path#anchor>` link claims two things: that the file still exists at
that repository-root-relative path, and that the anchor text still appears in
it, exactly once. A `<doc:page.md#Heading>` link claims the sibling page holds
that heading. Neither claim is checked by anything else, so both rot silently.

Run it over a repository root:

    python3 scripts/check-doc-links.py .

It exits non-zero when any link is broken or any anchor is ambiguous.
"""
from __future__ import annotations

import argparse
import pathlib
import re
import sys
import urllib.parse

CODE_LINK = re.compile(r"<code:([^#>]+)#([^>]*)>")
ENCODED_CODE_LINK = re.compile(r"\]\(code:([^#)\s]+)#([^)\s]*)\)")
DOC_LINK = re.compile(r"<doc:([^#>]+)#([^>]*)>")

SKIPPED_DIRECTORIES = (".claude/worktrees", "node_modules", "/target/", "/.git/")


def markdown_pages(repository_root: pathlib.Path) -> list[pathlib.Path]:
    return [
        page
        for page in sorted(repository_root.rglob("*.md"))
        if not any(skipped in str(page) for skipped in SKIPPED_DIRECTORIES)
    ]


def check_code_links(page: pathlib.Path, repository_root: pathlib.Path,
                     text: str) -> list[str]:
    problems = []
    relative_page = page.relative_to(repository_root)
    angle_bracket = [(one.group(1), one.group(2))
                     for one in CODE_LINK.finditer(text)]
    percent_encoded = [(urllib.parse.unquote(one.group(1)),
                        urllib.parse.unquote(one.group(2)))
                       for one in ENCODED_CODE_LINK.finditer(text)]
    if percent_encoded:
        problems.append(
            f"{relative_page}: {len(percent_encoded)} percent-encoded code "
            f"links, the angle-bracket spelling is canonical")
    for target, anchor in angle_bracket + percent_encoded:
        destination = repository_root / target
        if not destination.exists():
            problems.append(f"{relative_page}: no such file {target}")
            continue
        if not anchor:
            continue
        occurrences = destination.read_text(errors="replace").count(anchor)
        if occurrences == 0:
            problems.append(f"{relative_page}: anchor gone {target}#{anchor}")
        elif occurrences > 1:
            problems.append(
                f"{relative_page}: anchor matches {occurrences} lines, "
                f"use a whole declaration line {target}#{anchor}")
    return problems


def check_doc_links(page: pathlib.Path, repository_root: pathlib.Path,
                    text: str) -> list[str]:
    problems = []
    relative_page = page.relative_to(repository_root)
    for match in DOC_LINK.finditer(text):
        target, heading = match.group(1), match.group(2)
        destination = page.parent / target
        if not destination.exists():
            problems.append(f"{relative_page}: no such page {target}")
            continue
        headings = re.findall(r"^#+\s+(.*)$",
                              destination.read_text(errors="replace"),
                              flags=re.MULTILINE)
        if heading and heading.lower() not in [one.lower() for one in headings]:
            problems.append(f"{relative_page}: no such heading {target}#{heading}")
    return problems


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("repository_root", type=pathlib.Path)
    arguments = parser.parse_args()
    repository_root = arguments.repository_root.resolve()

    problems: list[str] = []
    checked = 0
    for page in markdown_pages(repository_root):
        text = page.read_text(errors="replace")
        checked += (len(CODE_LINK.findall(text))
                    + len(ENCODED_CODE_LINK.findall(text))
                    + len(DOC_LINK.findall(text)))
        problems += check_code_links(page, repository_root, text)
        problems += check_doc_links(page, repository_root, text)

    for problem in problems:
        print(problem)
    print(f"\nchecked {checked} links, {len(problems)} problems")
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
