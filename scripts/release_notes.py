#!/usr/bin/env python3
"""Read the current release from changelog.md (Python 3.11+)."""

import os
from pathlib import Path
import re
import tomllib


def parse_release(text, cargo_version):
    title, separator, body = text.strip().partition("\n")
    match = re.fullmatch(r"# (v((?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)))(?: +\S.*)?", title)
    if not match:
        raise ValueError("changelog.md must start with '# vX.Y.Z' and an optional title (stable versions only)")
    tag, version = match.groups()
    if version != cargo_version:
        raise ValueError(f"changelog version {version} differs from Cargo version {cargo_version}")
    if not separator or not body.strip():
        raise ValueError("changelog.md must contain release notes below the title")
    return tag, version, title[2:], body.strip() + "\n"


def main():
    manifest = tomllib.loads(Path("Cargo.toml").read_text(encoding="utf-8"))
    tag, version, title, body = parse_release(
        Path("changelog.md").read_text(encoding="utf-8"),
        manifest["workspace"]["package"]["version"],
    )
    lock = tomllib.loads(Path("Cargo.lock").read_text(encoding="utf-8"))
    for name in ("rospm-cli", "rospm-core"):
        if not any(p["name"] == name and p["version"] == version for p in lock["package"]):
            raise ValueError(f"Cargo.lock must contain {name} {version}")
    Path("release-notes.md").write_text(body, encoding="utf-8")
    with open(os.environ["GITHUB_OUTPUT"], "a", encoding="utf-8") as output:
        output.write(f"tag={tag}\nversion={version}\ntitle={title}\n")


if __name__ == "__main__":
    main()
