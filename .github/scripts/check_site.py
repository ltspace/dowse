"""Dependency-free checks for the static Dowse website.

The script intentionally validates only durable SEO contracts: one title and H1,
a useful description, a canonical URL, valid JSON-LD, resolvable local links, and
agreement between page canonicals and sitemap entries.
"""

from __future__ import annotations

import json
import sys
from html.parser import HTMLParser
from pathlib import Path
from urllib.parse import urlparse
from xml.etree import ElementTree


ROOT = Path(__file__).resolve().parents[2]
SITE = ROOT / "site"
BASE_URL = "https://lter.space/dowse/"


class PageParser(HTMLParser):
    def __init__(self) -> None:
        super().__init__()
        self.title_parts: list[str] = []
        self.in_title = False
        self.h1_count = 0
        self.description: str | None = None
        self.canonical: str | None = None
        self.hrefs: list[str] = []
        self.json_ld: list[str] = []
        self.in_json_ld = False
        self.json_ld_parts: list[str] = []

    def handle_starttag(self, tag: str, attrs: list[tuple[str, str | None]]) -> None:
        values = dict(attrs)
        if tag == "title":
            self.in_title = True
        elif tag == "h1":
            self.h1_count += 1
        elif tag == "meta" and values.get("name") == "description":
            self.description = values.get("content")
        elif tag == "link" and values.get("rel") == "canonical":
            self.canonical = values.get("href")
        elif tag == "a" and values.get("href"):
            self.hrefs.append(values["href"] or "")
        elif tag == "script" and values.get("type") == "application/ld+json":
            self.in_json_ld = True
            self.json_ld_parts = []

    def handle_endtag(self, tag: str) -> None:
        if tag == "title":
            self.in_title = False
        elif tag == "script" and self.in_json_ld:
            self.json_ld.append("".join(self.json_ld_parts))
            self.in_json_ld = False
            self.json_ld_parts = []

    def handle_data(self, data: str) -> None:
        if self.in_title:
            self.title_parts.append(data)
        if self.in_json_ld:
            self.json_ld_parts.append(data)


def fail(errors: list[str], path: Path, message: str) -> None:
    errors.append(f"{path.relative_to(ROOT)}: {message}")


def local_target(page: Path, href: str) -> Path | None:
    parsed = urlparse(href)
    if parsed.scheme or parsed.netloc or href.startswith(("#", "mailto:")):
        return None
    target = (page.parent / parsed.path).resolve()
    if parsed.path.endswith("/") or target.is_dir():
        target /= "index.html"
    return target


def main() -> int:
    errors: list[str] = []
    pages = sorted(SITE.rglob("*.html"))
    canonicals: set[str] = set()

    for page in pages:
        parser = PageParser()
        parser.feed(page.read_text(encoding="utf-8"))
        title = "".join(parser.title_parts).strip()
        if not 10 <= len(title) <= 70:
            fail(errors, page, f"title length should be 10..70 characters, got {len(title)}")
        if not parser.description or not 45 <= len(parser.description) <= 180:
            fail(errors, page, "meta description is missing or outside 45..180 characters")
        if parser.h1_count != 1:
            fail(errors, page, f"expected exactly one H1, got {parser.h1_count}")
        if not parser.canonical or not parser.canonical.startswith(BASE_URL):
            fail(errors, page, "canonical must use the production /dowse/ URL")
        elif parser.canonical in canonicals:
            fail(errors, page, f"duplicate canonical {parser.canonical}")
        else:
            canonicals.add(parser.canonical)

        for block in parser.json_ld:
            try:
                json.loads(block)
            except json.JSONDecodeError as exc:
                fail(errors, page, f"invalid JSON-LD: {exc}")

        for href in parser.hrefs:
            target = local_target(page, href)
            if target is not None and not target.exists():
                fail(errors, page, f"broken local link {href!r}")

    manifest = SITE / "manifest.webmanifest"
    json.loads(manifest.read_text(encoding="utf-8"))

    sitemap = ElementTree.parse(SITE / "sitemap.xml")
    namespace = {"sm": "http://www.sitemaps.org/schemas/sitemap/0.9"}
    sitemap_urls = {element.text for element in sitemap.findall("sm:url/sm:loc", namespace)}
    missing = canonicals - sitemap_urls
    extra = sitemap_urls - canonicals
    if missing:
        errors.append(f"sitemap.xml: missing canonicals: {sorted(missing)}")
    if extra:
        errors.append(f"sitemap.xml: URLs without a canonical page: {sorted(extra)}")

    robots = (SITE / "robots.txt").read_text(encoding="utf-8")
    if f"Sitemap: {BASE_URL}sitemap.xml" not in robots:
        errors.append("site/robots.txt: production sitemap declaration is missing")

    if errors:
        print("SEO checks failed:")
        for error in errors:
            print(f"- {error}")
        return 1

    print(f"SEO checks passed for {len(pages)} HTML pages and {len(canonicals)} canonical URLs.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
