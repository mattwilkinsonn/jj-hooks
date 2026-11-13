from __future__ import annotations

import subprocess
import sys


MAX_ATTEMPTS = 3
RETRYABLE_OUTPUT = ("non-fast-forward", "fetch first")


def main() -> int:
    for attempt in range(1, MAX_ATTEMPTS + 1):
        print(f"push attempt {attempt}/{MAX_ATTEMPTS}")
        subprocess.run(
            ["git", "fetch", "origin", "main:refs/remotes/origin/main"],
            check=True,
        )
        subprocess.run(["git", "rebase", "origin/main"], check=True)
        push = subprocess.run(
            ["git", "push", "origin", "HEAD:main"],
            capture_output=True,
            text=True,
        )
        output = push.stdout + push.stderr
        if push.returncode == 0:
            print(output, end="")
            return 0
        print(output, end="", file=sys.stderr)
        if not any(marker in output for marker in RETRYABLE_OUTPUT):
            return push.returncode
        if attempt == MAX_ATTEMPTS:
            print(
                f"error: tap push still rejected after {MAX_ATTEMPTS} attempts",
                file=sys.stderr,
            )
            return push.returncode
        print("tap main advanced; retrying after fetch and rebase", file=sys.stderr)
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
