from __future__ import annotations

import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


SCRIPT = Path(__file__).with_name("bump-formulae.py")
FORMULA = "Formula/jj-hooks.rb"


FORMULA_TEXT = '''class JjHooks < Formula
  desc "Hooks for jj"
  homepage "https://github.com/RigelBuild/jj-hooks"
  version "0.1.0"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/RigelBuild/jj-hooks/releases/download/v0.1.0/jj-hooks-darwin-arm64.tar.gz"
      sha256 "darwin-arm64-old"
    end
  end

  on_linux do
    if Hardware::CPU.intel?
      url "https://github.com/RigelBuild/jj-hooks/releases/download/v0.1.0/jj-hooks-linux-x64.tar.gz"
      sha256 "linux-x64-old"
    end

    if Hardware::CPU.arm?
      url "https://github.com/RigelBuild/jj-hooks/releases/download/v0.1.0/jj-hooks-linux-arm64.tar.gz"
      sha256 "linux-arm64-old"
    end
  end

  def install
    bin.install "jj-hooks"
  end
end
'''


class BumpFormulaeTest(unittest.TestCase):
    def run_script(self, formula_text: str) -> tuple[subprocess.CompletedProcess[str], bytes]:
        with tempfile.TemporaryDirectory() as directory:
            formula_path = Path(directory) / FORMULA
            formula_path.parent.mkdir()
            formula_path.write_text(formula_text)
            original_bytes = formula_path.read_bytes()

            environment = os.environ.copy()
            environment.update(
                {
                    "VER": "0.2.0",
                    "JJ_HOOKS_DARWIN_ARM64": "darwin-arm64-new",
                    "JJ_HOOKS_LINUX_X64": "linux-x64-new",
                    "JJ_HOOKS_LINUX_ARM64": "linux-arm64-new",
                }
            )
            result = subprocess.run(
                [sys.executable, str(SCRIPT)],
                cwd=directory,
                env=environment,
                text=True,
                capture_output=True,
                check=False,
            )
            return result, (formula_path.read_bytes() if formula_path.exists() else original_bytes)

    def test_rewrites_version_and_all_platform_shas(self) -> None:
        result, rewritten_bytes = self.run_script(FORMULA_TEXT)

        self.assertEqual(result.returncode, 0)
        expected = (
            FORMULA_TEXT.replace('version "0.1.0"', 'version "0.2.0"')
            .replace('sha256 "darwin-arm64-old"', 'sha256 "darwin-arm64-new"')
            .replace('sha256 "linux-x64-old"', 'sha256 "linux-x64-new"')
            .replace('sha256 "linux-arm64-old"', 'sha256 "linux-arm64-new"')
        ).encode()
        self.assertEqual(rewritten_bytes, expected)

    def test_version_anchor_drift_fails_without_writing(self) -> None:
        formula_text = FORMULA_TEXT.replace('version "0.1.0"', "version '0.1.0'")

        result, final_bytes = self.run_script(formula_text)

        self.assertEqual(result.returncode, 1)
        self.assertEqual(final_bytes, formula_text.encode())

    def test_sha_url_anchor_drift_fails_without_writing(self) -> None:
        formula_text = FORMULA_TEXT.replace(
            "jj-hooks/releases/download/v0.1.0/jj-hooks-linux-x64.tar.gz",
            "jj-hooks/releases/download/v0.1.0/jj-hooks-linux-x86_64.tar.gz",
        )

        result, final_bytes = self.run_script(formula_text)

        self.assertEqual(result.returncode, 1)
        self.assertEqual(final_bytes, formula_text.encode())


if __name__ == "__main__":
    unittest.main()
