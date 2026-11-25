#!/usr/bin/env python3
"""Write CHANGELOG.md from the commit history, grouped by area.

    python3 tools/changelog.py [--since TAG] [--until REV] > CHANGELOG.md

Each commit's subject is put under the first area whose pattern it
matches; a release is the commits since the last tag (all of them before
the first). The subjects are written as the commits say them, so a good
subject is a good line of the changelog.
"""

import re
import subprocess
import sys

AREAS = [
    # The first that matches wins, so the narrower areas come first.
    ("Operations", r"docker|self-hosting|backup|restore|upgrading|changelog|healthcheck|books integrity"),
    ("Admin UI", r"^(admin ui|astryx ui|grid):|astryx|shadcn|@erpcraft/grid|the admin ui"),
    ("Tax", r"\btax\b|\bvat\b|reverse charge|use tax"),
    ("Electronic invoices and EDI", r"\bedi\b|e-invoic|zugferd|xrechnung|x12|edifact"),
    (
        "Accounting",
        r"general ledger|\bperiods?\b|\byear\b|exchange rate|consolidation|intercompany|budget|"
        r"cash flow|journal",
    ),
    (
        "Manufacturing and planning",
        r"absorb|routing|work center|bill of materials|planning|reorder|draft production|manufactur",
    ),
    (
        "Inventory and warehousing",
        r"\bstock\b|\blots?\b|serial|physical inventory|cycle count|warehouse|\bsites?\b|transfer|"
        r"reserv|units of measure|availability",
    ),
    ("Purchasing and payables", r"purchas|vendor|payment run|nacha|sepa|await a credit note"),
    ("Sales and receivables", r"\bsales\b|customer|collections|dunning|price list|pricing"),
    ("Payments and banking", r"payment|\bbank|write off|open items|due in parts"),
    ("Printing and mail", r"print|\bpdfs?\b|letterhead|\bmail\b|email"),
    ("Platform", r"."),
]


def git(*args):
    return subprocess.run(["git", *args], check=True, capture_output=True, text=True).stdout


def area(subject):
    lowered = subject.lower()
    for name, pattern in AREAS:
        if re.search(pattern, lowered):
            return name
    return "Other"


def main():
    args = dict(zip(sys.argv[1::2], sys.argv[2::2]))
    since = args.get("--since")
    until = args.get("--until", "HEAD")
    if since is None:
        tags = git("tag", "--sort=-creatordate").split()
        since = tags[0] if tags else None
    revs = f"{since}..{until}" if since else until
    log = git("log", "--reverse", "--no-merges", "--format=%h%x09%ad%x09%s", "--date=short", revs)
    grouped = {}
    days = []
    for line in log.splitlines():
        sha, day, subject = line.split("\t", 2)
        days.append(day)
        grouped.setdefault(area(subject), []).append((sha, day, subject))
    version = git("describe", "--tags", "--abbrev=0").strip() if since else None
    out = ["# Changelog", ""]
    out.append(
        "What changed, by area, from the commit history (`python3 tools/changelog.py`). "
        "Upgrading between versions: see [docs/ops/UPGRADING.md](docs/ops/UPGRADING.md)."
    )
    out.append("")
    title = f"Unreleased (since {version})" if version else "0.1.0 (unreleased)"
    span = f"{min(days)} to {max(days)}" if days else "no commits"
    out += [f"## {title}", "", f"{len(days)} commits, {span}.", ""]
    for name, _ in AREAS + [("Other", "")]:
        commits = grouped.get(name)
        if not commits:
            continue
        out += [f"### {name}", ""]
        for sha, day, subject in commits:
            out.append(f"- {subject} ({sha}, {day})")
        out.append("")
    sys.stdout.write("\n".join(out))


if __name__ == "__main__":
    main()
