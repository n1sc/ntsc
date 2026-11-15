import json
import os
import subprocess
import tempfile
import tomllib
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
WORKFLOWS = ROOT / ".github/workflows"


def step_script(workflow, marker):
    section = (WORKFLOWS / workflow).read_text().split(f"      - {marker}\n", 1)[1]
    script = section.split("        run: |\n", 1)[1]
    lines = []
    for line in script.splitlines():
        if line and not line.startswith("          "):
            break
        lines.append(line[10:] if line else "")
    return "\n".join(lines)


def run_step(workflow, marker, **overrides):
    with tempfile.TemporaryDirectory(prefix="ntsc-release-test-") as directory:
        output = Path(directory) / "output"
        output.touch()
        environment = {
            **os.environ,
            "GITHUB_OUTPUT": str(output),
            "GITHUB_REF_NAME": "main",
            "GITHUB_REF_TYPE": "branch",
            "INPUT_VERSION": "",
            "INPUT_TAG": "",
            "INPUT_REF": "",
            "DRY_RUN": "true",
            "RUN_TESTS": "true",
            "WINDOWS_ONLY": "false",
            "PUBLISH": "true",
            **overrides,
        }
        result = subprocess.run(
            ["bash", "-eu", "-c", step_script(workflow, marker)],
            cwd=ROOT,
            env=environment,
            text=True,
            capture_output=True,
            check=False,
        )
        outputs = dict(line.split("=", 1) for line in output.read_text().splitlines())
        return result, outputs


class BinaryReleaseTests(unittest.TestCase):
    def test_branch_dry_run_uses_cargo_version_and_disables_publishing(self):
        result, outputs = run_step("release.yml", "id: flags")
        self.assertEqual(result.returncode, 0, result.stderr)
        cargo = tomllib.loads((ROOT / "Cargo.toml").read_text())
        self.assertEqual(outputs["version"], cargo["workspace"]["package"]["version"])
        self.assertEqual(outputs["dry_run"], "true")
        self.assertEqual(outputs["publish"], "false")
        self.assertEqual(outputs["run_tests"], "true")
        names = {entry["name"] for entry in json.loads(outputs["matrix"])["include"]}
        self.assertEqual(names, {
            "linux-x86_64", "macos-arm64", "macos-x86_64", "windows-x86_64",
        })

    def test_dry_run_can_override_version_and_select_windows(self):
        result, outputs = run_step(
            "release.yml", "id: flags", INPUT_VERSION="0.2.0", WINDOWS_ONLY="true",
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(outputs["version"], "0.2.0")
        self.assertEqual(outputs["publish"], "false")
        matrix = json.loads(outputs["matrix"])["include"]
        self.assertEqual([entry["name"] for entry in matrix], ["windows-x86_64"])

    def test_tag_push_still_releases(self):
        result, outputs = run_step(
            "release.yml", "id: flags",
            DRY_RUN="", GITHUB_REF_TYPE="tag", GITHUB_REF_NAME="v0.2.0",
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(outputs["version"], "0.2.0")
        self.assertEqual(outputs["dry_run"], "false")
        self.assertEqual(outputs["publish"], "true")

    def test_branch_cannot_create_a_release(self):
        result, outputs = run_step("release.yml", "id: flags", DRY_RUN="false")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("branch dispatches require dry_run=true", result.stdout)
        self.assertFalse(outputs)

    def test_release_job_is_gated(self):
        workflow = (WORKFLOWS / "release.yml").read_text()
        release = workflow.split("\n  release:\n", 1)[1].split("\n  publish-homebrew:", 1)[0]
        self.assertIn("needs: [config, build]", release)
        self.assertIn("if: needs.config.outputs.dry_run != 'true'", release)
        self.assertIn("default: true", workflow.split("      dry_run:\n", 1)[1])


class ExtensionReleaseTests(unittest.TestCase):
    def test_branch_dry_run_does_not_require_a_tag(self):
        result, outputs = run_step("extensions-release.yml", "id: meta", INPUT_REF="main")
        self.assertEqual(result.returncode, 0, result.stderr)
        release = json.loads((ROOT / "extensions/shared/release.json").read_text())
        self.assertEqual(outputs["version"], release["version"])
        self.assertEqual(outputs["tag"], "")
        commit = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()
        self.assertEqual(outputs["ref"], commit)

    def test_dry_run_tag_must_match_metadata(self):
        result, _ = run_step(
            "extensions-release.yml", "id: meta", INPUT_TAG="extensions-v999.0.0",
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("does not match", result.stderr)

    def test_branch_ref_cannot_be_published(self):
        result, outputs = run_step(
            "extensions-release.yml", "id: meta", DRY_RUN="false", INPUT_REF="main",
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("branch or commit inputs require dry_run=true", result.stderr)
        self.assertFalse(outputs)

    def test_non_dry_run_still_requires_an_extension_tag(self):
        result, outputs = run_step("extensions-release.yml", "id: meta", DRY_RUN="false")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("invalid tag: main", result.stderr)
        self.assertFalse(outputs)

    def test_actual_release_keeps_strict_grammar_validation(self):
        script = step_script("extensions-release.yml", "name: Verify release metadata")
        self.assertIn('if [ "$DRY_RUN" = "true" ]; then', script)
        self.assertIn('check --strict-grammar --expect-tag "$RELEASE_TAG"', script)
        workflow = (WORKFLOWS / "extensions-release.yml").read_text()
        self.assertEqual(workflow.count("if: ${{ !inputs.dry_run }}"), 2)
        self.assertEqual(workflow.count("ref: ${{ needs.meta.outputs.ref }}"), 3)
        self.assertIn("default: true", workflow.split("      dry_run:\n", 1)[1])


if __name__ == "__main__":
    unittest.main()
