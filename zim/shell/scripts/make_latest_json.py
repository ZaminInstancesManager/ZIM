#!/usr/bin/env python3
"""Assemble the update channel's latest.json from release artifacts.

Usage: make_latest_json.py <version> <artifacts-dir> <out-file>

Pairs each platform's updater payload with its minisign signature
(the `.sig` the Tauri bundler produced next to it) and writes the
manifest the shell's updater endpoint consumes. The URLs point at
the GitHub release assets for the given tag.
"""

import datetime
import json
import pathlib
import sys

REPO = "ZaminInstancesManager/ZIM"

# artifact-name → (updater platform key, payload filename suffixes).
# M27: the full §1 desktop matrix — Linux arm64 and macOS x64 joined
# the channel (their payloads ride the same minisign discipline).
# The darwin payloads are ARCH-QUALIFIED (`_x64` / `_aarch64`):
# Tauri names both legs' updater payload `Zamin.ZIM.app.tar.gz`, and
# the release upload collapses same-named assets into one — the
# workflow's collect step renames them so the channel cannot point
# both darwin platforms at one arch's bytes (alpha.2's first
# publish did exactly that, caught by the signature mismatch).
WANTED = {
    "zim-app-windows-x86_64": ("windows-x86_64", ("-setup.exe",)),
    "zim-app-linux-x86_64": ("linux-x86_64", (".AppImage",)),
    "zim-app-linux-aarch64": ("linux-aarch64", (".AppImage",)),
    "zim-app-darwin-aarch64": ("darwin-aarch64", ("_aarch64.app.tar.gz",)),
    "zim-app-darwin-x86_64": ("darwin-x86_64", ("_x64.app.tar.gz",)),
}


def main() -> int:
    if len(sys.argv) != 4:
        print(__doc__, file=sys.stderr)
        return 2
    version, artifacts_root, out_path = sys.argv[1], pathlib.Path(sys.argv[2]), sys.argv[3]

    platforms = {}
    for artifact_dir, (platform, suffixes) in WANTED.items():
        files = list((artifacts_root / artifact_dir).iterdir())
        for suffix in suffixes:
            payloads = [
                f for f in files
                if f.name.endswith(suffix) and not f.name.endswith(".sig")
            ]
            signatures = [f for f in files if f.name.endswith(suffix + ".sig")]
            if payloads and signatures:
                platforms[platform] = {
                    "signature": signatures[0].read_text().strip(),
                    "url": f"https://github.com/{REPO}/releases/download/v{version}/{payloads[0].name}",
                }

    if not platforms:
        print("no updater payload/signature pairs found", file=sys.stderr)
        return 1

    manifest = {
        "version": version,
        "notes": (
            f"ZIM alpha {version}. See the release page for what "
            "works and what does not — this is not production-ready software."
        ),
        "pub_date": datetime.datetime.now(datetime.timezone.utc).isoformat(),
        "platforms": platforms,
    }
    pathlib.Path(out_path).write_text(json.dumps(manifest, indent=2) + "\n")
    print(json.dumps(manifest, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
