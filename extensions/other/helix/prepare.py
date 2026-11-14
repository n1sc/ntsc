#!/usr/bin/env python3
"""Write a local Helix language configuration without changing user settings."""

from pathlib import Path
import json
import re

ROOT = Path(__file__).resolve().parents[3]
SOURCE = Path(__file__).resolve().parent / "languages.toml"


def main():
    output = ROOT / "target/editors/helix-languages.toml"
    output.parent.mkdir(parents=True, exist_ok=True)
    grammar = ROOT / "extensions/shared/tree-sitter-neutron"
    content = re.sub(r"source = \{.*\}", "source = { path = " + json.dumps(str(grammar)) + " }", SOURCE.read_text())
    output.write_text(content)
    print("Merge this generated configuration into ~/.config/helix/languages.toml:")
    print(output)
    print("Copy runtime/queries/neutron to ~/.config/helix/runtime/queries/neutron.")
    print("Then run hx --grammar build.")


if __name__ == "__main__":
    main()
