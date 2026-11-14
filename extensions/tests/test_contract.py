import importlib.util
import json
import os
import plistlib
import subprocess
import sys
import tomllib
import unittest
from pathlib import Path
from unittest.mock import patch
from types import SimpleNamespace

ROOT = Path(__file__).resolve().parents[2]
EXTENSIONS = ROOT / "extensions"
CONTRACT = json.loads((EXTENSIONS / "shared/contract.json").read_text())


class ContractTests(unittest.TestCase):
    def test_generated_files_are_current(self):
        subprocess.run([sys.executable, str(EXTENSIONS / "shared/generate.py"), "--check"], check=True)

    def test_shared_versions_and_grammar_pins(self):
        subprocess.run(
            [sys.executable, "-B", str(ROOT / "scripts/extensions-version.py"), "check"],
            check=True,
        )

    def test_release_tag_matches_version(self):
        tag = os.environ.get("EXTENSIONS_RELEASE_TAG")
        if not tag:
            self.skipTest("EXTENSIONS_RELEASE_TAG is not set")
        subprocess.run(
            [
                sys.executable,
                "-B",
                str(ROOT / "scripts/extensions-version.py"),
                "check",
                "--expect-tag",
                tag,
            ],
            check=True,
        )

    def test_strict_grammar_pin(self):
        if os.environ.get("EXTENSIONS_STRICT_GRAMMAR") != "1":
            self.skipTest("EXTENSIONS_STRICT_GRAMMAR is not set")
        subprocess.run(
            [
                sys.executable,
                "-B",
                str(ROOT / "scripts/extensions-version.py"),
                "check",
                "--strict-grammar",
            ],
            check=True,
        )

    def test_zed_license_is_mit(self):
        self.assertTrue((EXTENSIONS / "zed/LICENSE").read_text().startswith("MIT License"))
        cargo = tomllib.loads((EXTENSIONS / "zed/Cargo.toml").read_text())
        self.assertEqual(cargo["package"]["license"], "MIT")

    def test_editor_files_are_valid(self):
        for file in EXTENSIONS.rglob("*"):
            if "node_modules" in file.parts or "target" in file.parts or not file.is_file():
                continue
            if file.suffix in {".json", ".sublime-settings", ".sublime-build", ".sublime-menu"}:
                json.loads(file.read_text())
            elif file.suffix == ".toml":
                tomllib.loads(file.read_text())
            elif file.suffix == ".tmLanguage":
                plistlib.loads(file.read_bytes())

    def test_client_identity_and_commands(self):
        package = json.loads((EXTENSIONS / "vscode/package.json").read_text())
        self.assertEqual(package["contributes"]["languages"][0]["id"], CONTRACT["language"]["id"])
        self.assertEqual(package["contributes"]["languages"][0]["extensions"], [CONTRACT["language"]["extension"]])
        helix = tomllib.loads((EXTENSIONS / "other/helix/languages.toml").read_text())
        self.assertEqual(helix["language"][0]["roots"], [CONTRACT["language"]["projectMarker"]])
        self.assertEqual(helix["language-server"]["ntsc"]["command"], CONTRACT["server"]["command"])
        self.assertEqual(helix["language-server"]["ntsc"]["args"], CONTRACT["server"]["args"])
        zed = tomllib.loads((EXTENSIONS / "zed/extension.toml").read_text())
        self.assertEqual(zed["language_servers"]["ntsc"]["language_ids"]["Neutron"], CONTRACT["language"]["id"])
        self.assertEqual(zed["grammars"]["neutron"]["path"], "extensions/shared/tree-sitter-neutron")
        tasks = json.loads((EXTENSIONS / "zed/languages/neutron/tasks.json").read_text())
        self.assertEqual([task["args"] for task in tasks], list(CONTRACT["workflows"].values()))
        sublime = json.loads((EXTENSIONS / "other/sublime/LSP-neutron.sublime-settings").read_text())
        self.assertEqual(sublime["command"], [CONTRACT["server"]["command"], *CONTRACT["server"]["args"]])

    def test_sublime_workflow_uses_argv_and_nearest_root(self):
        import tempfile
        calls = []
        errors = []
        with tempfile.TemporaryDirectory(prefix="neutron editor ") as root:
            nested = Path(root) / "child"
            nested.mkdir()
            (Path(root) / "neutron.toml").write_text("")
            (nested / "neutron.toml").write_text("")
            view = SimpleNamespace(file_name=lambda: str(nested / "main.nt"), is_dirty=lambda: False)
            window = SimpleNamespace(
                active_view=lambda: view, views=lambda: [view], folders=lambda: [root],
                run_command=lambda name, args=None: calls.append((name, args)),
            )
            sublime = SimpleNamespace(
                load_resource=lambda _name: json.dumps(CONTRACT),
                load_settings=lambda _name: {"cli": "/compiler path/ntsc"},
                error_message=errors.append,
            )
            modules = {
                "sublime": sublime,
                "sublime_plugin": SimpleNamespace(WindowCommand=object),
                "LSP": SimpleNamespace(),
                "LSP.plugin": SimpleNamespace(AbstractPlugin=object, register_plugin=lambda _p: None, unregister_plugin=lambda _p: None),
            }
            with patch.dict(sys.modules, modules):
                spec = importlib.util.spec_from_file_location("neutron_sublime", EXTENSIONS / "other/sublime/plugin.py")
                module = importlib.util.module_from_spec(spec)
                spec.loader.exec_module(module)
            command = module.NeutronWorkflowCommand()
            command.window = window
            command.run("test")
            self.assertFalse(errors)
            self.assertEqual(calls, [
                ("save_all", None),
                ("exec", {"cmd": ["/compiler path/ntsc", "test"], "working_dir": str(nested)}),
            ])
            command.run("unknown")
            self.assertEqual(len(calls), 2)
            self.assertTrue(errors)


if __name__ == "__main__":
    unittest.main()
