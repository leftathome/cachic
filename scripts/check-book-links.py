#!/usr/bin/env python3
"""Every page named in SUMMARY.md must exist in the built book.

mdBook silently skips a SUMMARY entry pointing outside its source directory: the link renders in
the sidebar, the page does not exist, and nobody notices until a reader clicks it. Checking the
built output rather than the source is the only way to catch that.
"""
import os
import re
import sys

BOOK = "target/book"

summary = open("docs/SUMMARY.md").read()
links = re.findall(r"\]\((\./[^)]+\.md)\)", summary)

missing = []
for link in links:
    relative = link[2:]
    candidates = [os.path.join(BOOK, relative.replace(".md", ".html"))]
    # mdBook renders README.md as the directory index - but only README.md. Applying this
    # fallback to every entry made the check match target/book/index.html for anything at the top
    # level, so it passed on a page that did not exist.
    if os.path.basename(relative).lower() == "readme.md":
        candidates.append(os.path.join(BOOK, os.path.dirname(relative), "index.html"))
    if not any(os.path.exists(c) for c in candidates):
        missing.append(link)

print(f"SUMMARY entries: {len(links)}, missing from the build: {len(missing)}")
for link in missing:
    print(f"  {link}")
sys.exit(1 if missing else 0)
