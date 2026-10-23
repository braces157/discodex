#!/usr/bin/env python3
"""Build the Discodex provider marks from the vendored sources.

Every mark is written as a 512x512 RGBA PNG with the same silhouette: a rounded
square plate at a fixed fraction of the canvas, with the artwork optically
centred inside it. That single geometry is what makes 30 logos from 30 vendors
read as one set in a Discord profile.

Two source shapes are handled:

* ``glyph`` - artwork on transparency (an OpenAI knot, a Claude starburst, a
  Cursor wedge). It is cropped to its ink bounds, fitted to the plate's inner
  safe area, and drawn on the shared dark plate.
* ``tile``  - artwork that is already a filled app tile with its own brand
  plate (Aider green, Kilo yellow, Kiro purple, ...). The plate is kept, its
  corners are rounded to the same radius, and its footprint matches the glyph
  plates, so a tile reads as a badge in the same grid.

Glyph marks get a small optical correction: dense marks shrink slightly and
thin marks grow slightly, so a solid disc and an outline carry similar weight.

The build is offline and deterministic. It reads tools/logos/sources/ and the
same inputs always produce the same bytes.

Usage:
    python tools/logos/build.py             # write assets/logos/
    python tools/logos/build.py --sheet     # also render the contact sheet
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import sys

from PIL import Image, ImageDraw

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(HERE))
SOURCES = os.path.join(HERE, "sources")
CONFIG = os.path.join(HERE, "sources.json")
OUT_DIR = os.path.join(ROOT, "assets", "logos")
SHEET = os.path.join(ROOT, "assets", "_preview", "logos-contact-sheet.png")

PLATE_SIZE = 0.86      # plate edge as a fraction of the canvas
PLATE_RADIUS = 0.235   # corner radius as a fraction of the plate edge
GLYPH_INSET = 0.60     # glyph edge as a fraction of the plate edge
TILE_INSET = 0.98      # a brand tile fills nearly all of its plate

REFERENCE_DENSITY = 0.45
DENSITY_EXPONENT = 0.20
MIN_CORRECTION = 0.92
MAX_CORRECTION = 1.08


def load_rgba(path: str) -> Image.Image:
    image = Image.open(path)
    image.load()
    return image.convert("RGBA")


def ink_bbox(image: Image.Image, threshold: int = 8) -> tuple[int, int, int, int]:
    """Bounding box of everything that is not (nearly) transparent."""
    alpha = image.getchannel("A").point(lambda value: 255 if value > threshold else 0)
    box = alpha.getbbox()
    if box is None:
        raise ValueError("source artwork is fully transparent")
    return box


def density(image: Image.Image) -> float:
    """Mean alpha of the artwork, 0..1. A proxy for how solid the mark reads."""
    histogram = image.getchannel("A").histogram()
    return sum(count * value for value, count in enumerate(histogram)) / (
        255.0 * image.width * image.height
    )


def fit(image: Image.Image, target: int) -> Image.Image:
    """Scale so the longest side of the artwork is ``target`` pixels."""
    width, height = image.size
    factor = target / max(width, height)
    size = (max(1, round(width * factor)), max(1, round(height * factor)))
    return image.resize(size, Image.LANCZOS)


def plate(canvas: int, colour: str) -> Image.Image:
    edge = round(canvas * PLATE_SIZE)
    mask = Image.new("L", (edge, edge), 0)
    ImageDraw.Draw(mask).rounded_rectangle(
        (0, 0, edge - 1, edge - 1), radius=round(edge * PLATE_RADIUS), fill=255
    )
    base = Image.new("RGBA", (edge, edge), colour)
    base.putalpha(mask)
    out = Image.new("RGBA", (canvas, canvas), (0, 0, 0, 0))
    offset = (canvas - edge) // 2
    out.alpha_composite(base, (offset, offset))
    return out


def place(canvas: int, artwork: Image.Image, reference: Image.Image) -> Image.Image:
    """Centre ``artwork`` on ``reference``, which already carries the plate."""
    out = reference.copy()
    out.alpha_composite(
        artwork,
        ((canvas - artwork.width) // 2, (canvas - artwork.height) // 2),
    )
    return out


def build_glyph(source: Image.Image, canvas: int, plate_colour: str) -> tuple[Image.Image, dict]:
    glyph = source.crop(ink_bbox(source))

    edge = round(canvas * PLATE_SIZE)
    target = round(edge * GLYPH_INSET)
    glyph = fit(glyph, target)

    # Dense artwork shrinks slightly, thin artwork grows slightly, so the set
    # carries similar optical weight at tray and profile sizes. The scale is
    # applied to the longest side, never to the width, so a tall mark and a
    # wide mark are corrected by the same rule.
    mass = density(glyph)
    correction = 1.0
    if mass > 0:
        correction = (REFERENCE_DENSITY / mass) ** DENSITY_EXPONENT
        correction = max(MIN_CORRECTION, min(MAX_CORRECTION, correction))
    if abs(correction - 1.0) > 0.005:
        longest = max(glyph.width, glyph.height)
        glyph = fit(glyph, max(1, round(longest * correction)))

    out = place(canvas, glyph, plate(canvas, plate_colour))
    return out, {
        "mode": "glyph",
        "ink": round(max(glyph.width, glyph.height) / canvas, 3),
        "density": round(mass, 3),
        "correction": round(correction, 3),
    }


def build_tile(source: Image.Image, canvas: int, fallback: str) -> tuple[Image.Image, dict]:
    tile = source.crop(ink_bbox(source))

    edge = round(canvas * PLATE_SIZE)
    tile = fit(tile, round(edge * TILE_INSET))

    if tile.width != tile.height:
        side = max(tile.width, tile.height)
        square = Image.new("RGBA", (side, side), fallback)
        square.alpha_composite(tile, ((side - tile.width) // 2, (side - tile.height) // 2))
        tile = square

    # A brand tile is opaque artwork; only its silhouette needs the shared
    # corner radius, which is applied by masking it onto a transparent canvas.
    mask = Image.new("L", tile.size, 0)
    ImageDraw.Draw(mask).rounded_rectangle(
        (0, 0, tile.width - 1, tile.height - 1),
        radius=round(tile.width * PLATE_RADIUS),
        fill=255,
    )
    tile.putalpha(mask)

    base = Image.new("RGBA", (canvas, canvas), (0, 0, 0, 0))
    out = place(canvas, tile, base)
    return out, {"mode": "tile", "ink": round(tile.width / canvas, 3), "correction": 1.0}


def source_path(slug: str) -> str:
    for name in sorted(os.listdir(SOURCES)):
        if os.path.splitext(name)[0] == slug:
            return os.path.join(SOURCES, name)
    raise FileNotFoundError(f"no vendored source for {slug!r}; run tools/logos/vendor.py")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--sheet", action="store_true", help="also render the contact sheet")
    args = parser.parse_args()

    config = json.load(open(CONFIG, encoding="utf-8"))
    canvas = config["canvas"]
    plate_colour = config["plate_color"]
    os.makedirs(OUT_DIR, exist_ok=True)

    records = []
    for entry in config["entries"]:
        slug = entry["slug"]
        path = source_path(slug)
        source = load_rgba(path)
        digest = hashlib.sha256(open(path, "rb").read()).hexdigest()

        if entry["mode"] == "tile":
            result, geometry = build_tile(source, canvas, plate_colour)
        else:
            result, geometry = build_glyph(source, canvas, plate_colour)

        target = os.path.join(OUT_DIR, slug + ".png")
        result.save(target, "PNG", optimize=True)

        records.append(
            {
                "slug": slug,
                "provider": entry["provider"],
                "file": f"assets/logos/{slug}.png",
                "source": entry["source"],
                "source_sha256": digest,
                **geometry,
                "bytes": os.path.getsize(target),
            }
        )
        print(
            f"  {slug:18} {geometry['mode']:5} ink={geometry['ink']:.2f} "
            f"corr={geometry['correction']:.2f} {os.path.getsize(target):>7} bytes"
        )

    manifest = {
        "canvas": canvas,
        "plate_size": PLATE_SIZE,
        "plate_radius": PLATE_RADIUS,
        "glyph_inset": GLYPH_INSET,
        "tile_inset": TILE_INSET,
        "plate_color": plate_colour,
        "marks": records,
    }
    with open(os.path.join(OUT_DIR, "manifest.json"), "w", encoding="utf-8") as handle:
        json.dump(manifest, handle, indent=2)
        handle.write("\n")

    print(f"\n{len(records)} marks -> {os.path.relpath(OUT_DIR, ROOT)}")

    if args.sheet:
        write_sheet(records, canvas)
    return 0


def write_sheet(records: list[dict], canvas: int) -> None:
    """Review sheet: every mark on both card tones, at profile and tray sizes."""
    columns = 6
    cell = 172
    label = 24
    rows = (len(records) + columns - 1) // columns
    width = columns * cell
    height = rows * (cell + label)

    sheet = Image.new("RGB", (width * 2, height), "#111111")
    sheet.paste(Image.new("RGB", (width, height), "#1E1F22"), (0, 0))
    sheet.paste(Image.new("RGB", (width, height), "#F2F2F2"), (width, 0))

    draw = ImageDraw.Draw(sheet)
    for index, record in enumerate(records):
        column = index % columns
        row = index // columns
        mark = Image.open(os.path.join(ROOT, record["file"])).convert("RGBA")
        big = mark.resize((110, 110), Image.LANCZOS)
        small = mark.resize((40, 40), Image.LANCZOS)
        for pane in (0, 1):
            x = pane * width + column * cell
            y = row * (cell + label)
            sheet.paste(big, (x + 14, y + 12), big)
            sheet.paste(small, (x + 128, y + 76), small)
        colour = "#8f9199" if record["mode"] == "glyph" else "#a08672"
        draw.text((column * cell + 14, row * (cell + label) + 126), record["slug"], fill=colour)
        draw.text(
            (width + column * cell + 14, row * (cell + label) + 126),
            record["slug"],
            fill=colour,
        )

    os.makedirs(os.path.dirname(SHEET), exist_ok=True)
    sheet.save(SHEET, "PNG", optimize=True)
    print(f"contact sheet -> {os.path.relpath(SHEET, ROOT)}")


if __name__ == "__main__":
    sys.exit(main())
