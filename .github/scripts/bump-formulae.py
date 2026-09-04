#!/usr/bin/env python3
"""Bump every Formula/*.rb to the current release.

Called from .github/workflows/release.yml's `bump-tap` job after
all platform builds have produced their SHA256 checksums.

Reads version + per-platform shas from env vars (passed by the
workflow step) and rewrites each formula's `version` line + each
per-slug `sha256` line in place. Anchors the sha256 rewrite on
the preceding `url ...` line so formulae with multiple
`on_macos`/`on_linux` blocks each get their own correct sha.

Env vars expected:

    VER                              - bare version (e.g. "0.3.0")
    <TOOL>_<SLUG>                    - e.g. JJ_HOOKS_DARWIN_ARM64
    where TOOL ∈ {JJ_HOOKS}
    and   SLUG ∈ {DARWIN_ARM64, LINUX_X64, LINUX_ARM64}

Each formula is rewritten only if at least one of its shas
changed (so a partial release that only built two of three
platforms doesn't corrupt the third's existing sha).
"""

from __future__ import annotations

import os
import re
import sys
from pathlib import Path

TOOLS = {
    "jj-hooks": "JJ_HOOKS",
}

SLUGS = {
    "darwin-arm64": "DARWIN_ARM64",
    "linux-x64": "LINUX_X64",
    "linux-arm64": "LINUX_ARM64",
}


def main() -> int:
    version = os.environ.get("VER", "").strip()
    if not version:
        print("error: VER env var not set", file=sys.stderr)
        return 1

    formula_dir = Path("Formula")
    any_changes = False

    for tool, env_prefix in TOOLS.items():
        formula_path = formula_dir / f"{tool}.rb"
        if not formula_path.exists():
            print(f"warn: no formula at {formula_path}; skipping", file=sys.stderr)
            continue

        shas: dict[str, str] = {}
        missing: list[str] = []
        for slug, env_slug in SLUGS.items():
            key = f"{env_prefix}_{env_slug}"
            value = os.environ.get(key, "").strip()
            if not value:
                missing.append(slug)
            else:
                shas[slug] = value
        if missing:
            print(
                f"error: missing {tool} sha256 env vars for slug(s): {', '.join(missing)}",
                file=sys.stderr,
            )
            return 1

        text = formula_path.read_text()
        original = text

        # version "X.Y.Z" → version "<new>"
        text = re.sub(
            r'^(\s*version\s+)"[^"]*"',
            rf'\1"{version}"',
            text,
            flags=re.M,
        )

        # For each slug: anchor on the preceding url line so the
        # sha256 directly below it is the one we replace. The
        # regex tolerates intervening blank or comment lines (the
        # initial formulae include a "# SHA256 is bumped by ..."
        # nudge above the first sha256, so without this we'd skip
        # the darwin-arm64 entry on the first release).
        for slug, sha in shas.items():
            pattern = re.compile(
                rf'(url\s+"[^"]*-{re.escape(slug)}\.tar\.gz"\s*\n'
                rf'(?:\s*(?:#[^\n]*)?\n)*'
                rf'\s+sha256\s+)"[^"]*"'
            )
            text = pattern.sub(rf'\1"{sha}"', text)

        if text != original:
            formula_path.write_text(text)
            print(f"bumped {formula_path}")
            any_changes = True
        else:
            print(f"no changes for {formula_path}")

    if not any_changes:
        print("no formula changes to commit")
    return 0


if __name__ == "__main__":
    sys.exit(main())
