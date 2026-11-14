#!/usr/bin/env python3
"""Prepare a development extension using a local, immutable grammar checkout."""

import shutil
import subprocess
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


def git(directory, *arguments):
    return subprocess.check_output(["git", "-C", str(directory), *arguments], text=True).strip()


def main():
    grammar = ROOT / "extensions/shared/tree-sitter-neutron"
    if not (grammar / "src/parser.c").exists():
        raise SystemExit("Generate the grammar first: npm run generate --prefix extensions/shared/tree-sitter-neutron")
    output = ROOT / "target/editors"
    output.mkdir(parents=True, exist_ok=True)
    directory = Path(tempfile.mkdtemp(prefix="neutron-", dir=output))
    snapshot = directory / "grammar"
    shutil.copytree(grammar, snapshot, ignore=shutil.ignore_patterns("node_modules", "*.so", "target"))
    git(snapshot, "init", "--quiet")
    git(snapshot, "add", ".")
    git(snapshot, "-c", "user.name=Neutron development", "-c", "user.email=dev@localhost",
        "commit", "--quiet", "-m", "build: snapshot local highlighting grammar")
    revision = git(snapshot, "rev-parse", "HEAD")
    extension = directory / "zed"
    shutil.copytree(ROOT / "extensions/zed", extension,
                    ignore=shutil.ignore_patterns("target", "grammars", "extension.wasm", "__pycache__"))
    manifest = (extension / "extension.toml").read_text()
    manifest = manifest.split("[grammars.neutron]")[0] + (
        f'[grammars.neutron]\nrepository = "{snapshot.as_uri()}"\nrev = "{revision}"\n'
    )
    (extension / "extension.toml").write_text(manifest)
    print("In Zed, run 'zed: install dev extension' and select:")
    print(extension)
    print("Only the throwaway grammar checkout under target/editors was committed.")


if __name__ == "__main__":
    main()
