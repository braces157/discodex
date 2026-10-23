#!/usr/bin/env python3
"""Fetch the source artwork for the provider marks into tools/logos/sources/.

This is a maintainer step, run only when a provider's artwork needs to change.
It is the one part of the pipeline that touches the network. ``build.py`` reads
the vendored PNGs and is fully offline, so a checkout always rebuilds the same
bytes.

SVG sources are rasterised here, at 1024px, with a headless Chromium/Edge
instance, so the build step only ever handles PNG.

Usage:
    python tools/logos/vendor.py             # fetch anything missing
    python tools/logos/vendor.py --force     # refetch everything
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import shutil
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
SOURCES = os.path.join(HERE, "sources")
MANIFEST = os.path.join(HERE, "sources.json")

USER_AGENT = (
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 "
    "(KHTML, like Gecko) Chrome/126.0 Safari/537.36"
)

BROWSERS = [
    r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
    r"C:\Program Files\Microsoft\Edge\Application\msedge.exe",
    r"C:\Program Files\Google\Chrome\Application\chrome.exe",
    "/usr/bin/google-chrome",
    "/usr/bin/chromium",
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
]

RASTER = 1024


def find_browser() -> str:
    override = os.environ.get("DISCODEX_CHROME")
    if override and os.path.exists(override):
        return override
    for candidate in BROWSERS:
        if os.path.exists(candidate):
            return candidate
    raise SystemExit(
        "No Chromium-based browser found for SVG rasterising. "
        "Install Edge/Chrome or set DISCODEX_CHROME to the executable."
    )


def rasterize(svg_bytes: bytes, out_png: str) -> None:
    """Render an SVG to a square, transparent PNG with a headless browser."""
    browser = find_browser()
    body = svg_bytes.decode("utf-8")
    body = body[body.index("<svg"):]
    html = (
        "<!doctype html><meta charset='utf-8'>"
        "<style>html,body{margin:0;background:transparent}"
        f"#b{{width:{RASTER}px;height:{RASTER}px;display:flex;align-items:center;justify-content:center}}"
        f"#b svg{{width:100%;height:100%}}</style>"
        f"<div id='b'>{body}</div>"
    )
    workdir = tempfile.mkdtemp(prefix="discodex-logo-")
    try:
        page = os.path.join(workdir, "page.html")
        with open(page, "w", encoding="utf-8") as handle:
            handle.write(html)
        subprocess.run(
            [
                browser,
                "--headless=new",
                "--disable-gpu",
                "--hide-scrollbars",
                "--default-background-color=00000000",
                "--force-device-scale-factor=1",
                f"--screenshot={out_png}",
                f"--window-size={RASTER},{RASTER}",
                "file:///" + page.replace(os.sep, "/"),
            ],
            capture_output=True,
            timeout=120,
            check=True,
        )
    finally:
        shutil.rmtree(workdir, ignore_errors=True)


def fetch(url: str) -> bytes:
    import urllib.request

    request = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
    with urllib.request.urlopen(request, timeout=60) as response:
        return response.read()


def is_svg(payload: bytes) -> bool:
    head = payload[:256].lstrip().lower()
    return head.startswith(b"<svg") or head.startswith(b"<?xml")


def apply_ink(svg: bytes, ink: str) -> bytes:
    """Force a single-colour SVG onto a known ink.

    Monochrome icon sets ship their glyph as ``currentColor`` or with no fill at
    all, which rasterises black. On a dark plate that is invisible, so the ink
    is written into the root element where every path inherits it.
    """
    text = svg.decode("utf-8")
    for attribute in ('fill="currentColor"', "fill='currentColor'"):
        text = text.replace(attribute, f'fill="{ink}"')
    if "fill=" not in text.split(">", 1)[0]:
        text = text.replace("<svg", f'<svg fill="{ink}"', 1)
    return text.encode("utf-8")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--force", action="store_true", help="refetch every source")
    args = parser.parse_args()

    config = json.load(open(MANIFEST, encoding="utf-8"))
    os.makedirs(SOURCES, exist_ok=True)

    failures = 0
    for entry in config["entries"]:
        slug = entry["slug"]
        target = os.path.join(SOURCES, slug + ".png")
        if os.path.exists(target) and not args.force:
            print(f"  skip  {slug:20} {os.path.getsize(target):>8} bytes")
            continue

        try:
            payload = fetch(entry["source"])
        except Exception as error:  # noqa: BLE001 - report and continue
            print(f"  FAIL  {slug:20} {error}", file=sys.stderr)
            failures += 1
            continue

        if is_svg(payload):
            if entry.get("ink"):
                payload = apply_ink(payload, entry["ink"])
            staged = target + ".tmp.png"
            rasterize(payload, staged)
            os.replace(staged, target)
            kind = "svg->png"
        else:
            with open(target, "wb") as handle:
                handle.write(payload)
            kind = "png"

        digest = hashlib.sha256(open(target, "rb").read()).hexdigest()[:16]
        print(f"  get   {slug:20} {os.path.getsize(target):>8} bytes  {kind:8} sha256:{digest}")

    return 1 if failures else 0


if __name__ == "__main__":
    raise SystemExit(main())
