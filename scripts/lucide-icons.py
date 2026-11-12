#!/usr/bin/env python3
"""Cuts the Lucide icon font down to the icons PrzmTerm uses.

    npm pack lucide-static && tar xzf lucide-static-*.tgz
    scripts/lucide-icons.py package

writes assets/fonts/lucide-icons.ttf (a few KiB instead of 1 MB) and prints
the code point of each icon for crates/przmterm-app/src/icons.rs. To add an
icon, add its name below (as on https://lucide.dev/icons), run this again
and add it to the `Icon` enum. Needs fontTools (pip install fonttools).
"""

import json
import pathlib
import subprocess
import sys

ICONS = [
    "chevron-left",
    "chevron-right",
    "chevron-down",
    "rotate-cw",
    "rotate-ccw",
    "lock",
    "lock-open",
    "settings",
    "plus",
    "x",
    "arrow-up",
    "grip-vertical",
    "folder",
    "square-terminal",
    "monitor",
    "cable",
    "laptop",
    "columns-2",
    "rows-2",
    "ellipsis",
    "plug",
    "unplug",
    "activity",
    "bell-dot",
    "file-pen-line",
    "file",
    "panel-bottom",
    "triangle-alert",
    "pencil",
    "folder-open",
    "trash-2",
    "eraser",
    "file-text",
]


def main() -> None:
    package = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else "package")
    font = package / "font"
    codepoints = json.loads((font / "codepoints.json").read_text())
    version = json.loads((package / "package.json").read_text())["version"]
    missing = [name for name in ICONS if name not in codepoints]
    if missing:
        sys.exit(f"not in Lucide {version}: {', '.join(missing)}")
    out = pathlib.Path(__file__).resolve().parent.parent / "assets/fonts/lucide-icons.ttf"
    unicodes = ",".join(f"U+{codepoints[name]:04X}" for name in ICONS)
    subprocess.run(
        [
            sys.executable,
            "-m",
            "fontTools.subset",
            str(font / "lucide.ttf"),
            f"--unicodes={unicodes}",
            f"--output-file={out}",
            "--no-hinting",
            "--layout-features=",
            "--name-IDs=*",
        ],
        check=True,
    )
    print(f"{out} from Lucide {version}:")
    for name in ICONS:
        print(f"  {name}: 0x{codepoints[name]:04X}")


if __name__ == "__main__":
    main()
