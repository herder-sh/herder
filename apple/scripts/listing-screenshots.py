#!/usr/bin/env python3
"""Frames the iPhone app's App Store screenshots: each shot apple/listing/listing.toml names,
under its caption, on herder.sh's dark background, at the 6.9" display's 1320×2868.

usage: apple/scripts/listing-screenshots.py <shots> <out>
  <shots>  the ScreenshotTests PNGs, such as the `screenshots` workflow's artifact
           (scripts/site-screenshots.sh shows how to fetch it)
  <out>    where the frames go, as 01-ios-home.png and so on, for app-store-listing.py

Needs ImageMagick's magick.
"""
import subprocess
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
WIDTH, HEIGHT = 1320, 2868
# herder.sh's Tokyo Night: its background, text and accent.
BG, FG, ACCENT, LINE = "#1a1b26", "#c0caf5", "#7aa2f7", "#2f334d"
INTER = ROOT / "site/fonts/inter.woff2"
MONO = ROOT / "site/fonts/jetbrains-mono.woff2"
# The shot, scaled to this width with rounded corners, sits below the caption.
SHOT_WIDTH, SHOT_TOP, CORNER = 1040, 500, 72


def frame(shot: Path, caption: str, out: Path) -> None:
    height = round(SHOT_WIDTH * 2868 / 1320)
    radius = CORNER
    subprocess.run(
        [
            "magick",
            "-size", f"{WIDTH}x{HEIGHT}", f"gradient:{BG}-#16161e",
            # The brand, as the site's header writes it, then the caption.
            "-font", str(MONO), "-fill", ACCENT, "-pointsize", "44",
            "-gravity", "north", "-annotate", "+0+130", "herder",
            "-font", str(INTER), "-fill", FG, "-stroke", FG, "-strokewidth", "2", "-pointsize", "84",
            "-size", f"{WIDTH - 160}x", "-background", "none",
            f"caption:{caption}", "-gravity", "north", "-geometry", "+0+220", "-composite",
            "-stroke", "none",
            # A hairline border, then the shot over it with its corners rounded.
            "(", "-size", f"{SHOT_WIDTH + 6}x{height + 6}", "xc:none", "-fill", LINE,
            "-draw", f"roundrectangle 0,0 {SHOT_WIDTH + 5},{height + 5} {radius + 3},{radius + 3}", ")",
            "-geometry", f"+0+{SHOT_TOP - 3}", "-composite",
            "(", str(shot), "-resize", f"{SHOT_WIDTH}x{height}!", "-alpha", "set",
            "(", "+clone", "-alpha", "transparent", "-fill", "white",
            "-draw", f"roundrectangle 0,0 {SHOT_WIDTH - 1},{height - 1} {radius},{radius}", ")",
            "-compose", "DstIn", "-composite", "-compose", "Over", ")",
            "-geometry", f"+0+{SHOT_TOP}", "-composite",
            # App Store Connect refuses screenshots with an alpha channel.
            "-alpha", "off", "-depth", "8", str(out),
        ],
        check=True,
    )


def main() -> None:
    if len(sys.argv) != 3:
        sys.exit(__doc__)
    shots, out = Path(sys.argv[1]), Path(sys.argv[2])
    listing = tomllib.loads((ROOT / "apple/listing/listing.toml").read_text())
    out.mkdir(parents=True, exist_ok=True)
    missing = []
    for index, entry in enumerate(listing["screenshots"], 1):
        shot = shots / f"{entry['shot']}.png"
        if not shot.exists():
            missing.append(entry["shot"])
            continue
        target = out / f"{index:02}-{entry['shot']}.png"
        frame(shot, entry["caption"], target)
        print(target.name)
    if missing:
        sys.exit(f"missing: {', '.join(missing)}")


if __name__ == "__main__":
    main()
