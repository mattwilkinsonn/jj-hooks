from __future__ import annotations

import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


SCRIPT = Path(__file__).with_name("push-tap.py")


FAKE_GIT = '''#!/usr/bin/env python3
import json
import os
import sys
from pathlib import Path

log = Path(os.environ["FAKE_GIT_LOG"])
commands = json.loads(log.read_text()) if log.exists() else []
commands.append(sys.argv[1:])
log.write_text(json.dumps(commands))
if sys.argv[1] == "push":
    outcomes = os.environ["PUSH_OUTCOMES"].split("|")
    index = sum(command[0] == "push" for command in commands) - 1
    outcome = outcomes[index]
    if ":" in outcome:
        status, output = outcome.split(":", 1)
    else:
        status, output = outcome, ""
    print(output, file=sys.stderr)
    raise SystemExit(int(status))
'''


class PushTapTest(unittest.TestCase):
    def run_helper(self, outcomes: str) -> tuple[subprocess.CompletedProcess[str], list[list[str]]]:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            fake_git = root / "git"
            fake_git.write_text(FAKE_GIT)
            fake_git.chmod(0o755)
            log = root / "commands.json"
            environment = os.environ | {
                "PATH": f"{root}{os.pathsep}{os.environ['PATH']}",
                "FAKE_GIT_LOG": str(log),
                "PUSH_OUTCOMES": outcomes,
            }
            result = subprocess.run(
                [sys.executable, str(SCRIPT)],
                capture_output=True,
                text=True,
                env=environment,
            )
            commands = json.loads(log.read_text()) if log.exists() else []
            return result, commands

    def test_fetch_first_retries_then_succeeds(self) -> None:
        result, commands = self.run_helper("1:fetch first|0:push succeeded")

        self.assertEqual(result.returncode, 0)
        self.assertEqual([command[0] for command in commands], ["fetch", "rebase", "push", "fetch", "rebase", "push"])
        self.assertEqual(commands.count(["push", "origin", "HEAD:main"]), 2)

    def test_unrelated_push_failure_is_immediate(self) -> None:
        result, commands = self.run_helper("7:permission denied|0:must not run")

        self.assertEqual(result.returncode, 7)
        self.assertEqual([command[0] for command in commands], ["fetch", "rebase", "push"])

    def test_retry_exhaustion_stops_after_three_pushes(self) -> None:
        result, commands = self.run_helper("1:non-fast-forward|1:non-fast-forward|1:non-fast-forward|0:must not run")

        self.assertEqual(result.returncode, 1)
        self.assertEqual([command[0] for command in commands], [
            "fetch", "rebase", "push", "fetch", "rebase", "push", "fetch", "rebase", "push",
        ])
        self.assertEqual(commands.count(["push", "origin", "HEAD:main"]), 3)


if __name__ == "__main__":
    unittest.main()
