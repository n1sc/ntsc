#!/usr/bin/env python3
"""Check and prepare shared metadata for the editor extension release."""

import argparse
import json
import os
import re
import subprocess
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
RELEASE_PATH = ROOT / "extensions/shared/release.json"
VERSION_RE = re.compile(r"^\d+\.\d+\.\d+$")
SHA_RE = re.compile(r"^[0-9a-f]{40}$")
GRAMMAR_SUBTREE = "extensions/shared/tree-sitter-neutron"
GRAMMAR_FILES = ("grammar.js", "src/parser.c")


def read(relative):
    return (ROOT / relative).read_text()


def write(relative, content):
    (ROOT / relative).write_text(content)


def load_release():
    return json.loads(RELEASE_PATH.read_text())


def git(*arguments, capture=False):
    result = subprocess.run(
        ["git", *arguments],
        cwd=ROOT,
        text=True,
        capture_output=capture,
        check=False,
    )
    if result.returncode != 0:
        message = result.stderr.strip() if capture else ""
        raise SystemExit(f"git {' '.join(arguments)} failed{': ' + message if message else ''}")
    return result.stdout.strip() if capture else ""


def replace_once(text, pattern, replacement, label):
    updated, count = re.subn(pattern, replacement, text, flags=re.MULTILINE)
    if count != 1:
        raise SystemExit(f"{label}: expected exactly one match, found {count}")
    return updated


def replace_all(text, pattern, replacement, label):
    updated, count = re.subn(pattern, replacement, text, flags=re.MULTILINE)
    if count == 0:
        raise SystemExit(f"{label}: expected at least one match, found none")
    return updated


def version_locations():
    vscode = json.loads(read("extensions/vscode/package.json"))
    vscode_lock = json.loads(read("extensions/vscode/package-lock.json"))
    zed = tomllib.loads(read("extensions/zed/extension.toml"))
    zed_cargo = tomllib.loads(read("extensions/zed/Cargo.toml"))
    tree_sitter = json.loads(read("extensions/shared/tree-sitter-neutron/package.json"))
    tree_sitter_lock = json.loads(read("extensions/shared/tree-sitter-neutron/package-lock.json"))
    tree_sitter_manifest = json.loads(read("extensions/shared/tree-sitter-neutron/tree-sitter.json"))
    emacs_version = re.search(r"^;; Version: (.+)$", read("extensions/emacs/neutron-mode.el"), re.MULTILINE)
    zed_lock_version = re.search(
        r'^name = "neutron-zed"\nversion = "([^"]+)"$',
        read("extensions/zed/Cargo.lock"),
        re.MULTILINE,
    )
    return [
        ("extensions/vscode/package.json", vscode["version"]),
        ("extensions/vscode/package-lock.json", vscode_lock["version"]),
        ("extensions/vscode/package-lock.json packages[\"\"]", vscode_lock["packages"][""]["version"]),
        ("extensions/zed/extension.toml", zed["version"]),
        ("extensions/zed/Cargo.toml", zed_cargo["package"]["version"]),
        ("extensions/zed/Cargo.lock", zed_lock_version.group(1) if zed_lock_version else "<missing>"),
        ("extensions/emacs/neutron-mode.el", emacs_version.group(1) if emacs_version else "<missing>"),
        ("extensions/shared/tree-sitter-neutron/package.json", tree_sitter["version"]),
        ("extensions/shared/tree-sitter-neutron/package-lock.json", tree_sitter_lock["version"]),
        (
            'extensions/shared/tree-sitter-neutron/package-lock.json packages[""]',
            tree_sitter_lock["packages"][""]["version"],
        ),
        ("extensions/shared/tree-sitter-neutron/tree-sitter.json", tree_sitter_manifest["metadata"]["version"]),
    ]


def grammar_locations(release):
    repository = release["grammar"]["repository"]
    rev = release["grammar"]["rev"]
    zed = tomllib.loads(read("extensions/zed/extension.toml"))
    helix = tomllib.loads(read("extensions/other/helix/languages.toml"))
    zed_grammar = zed["grammars"]["neutron"]
    helix_source = helix["grammar"][0]["source"]
    return [
        ("extensions/zed/extension.toml grammar repository", zed_grammar["repository"], repository),
        ("extensions/zed/extension.toml grammar rev", zed_grammar["rev"], rev),
        ("extensions/other/helix/languages.toml grammar repository", helix_source["git"], f"{repository}.git"),
        ("extensions/other/helix/languages.toml grammar rev", helix_source["rev"], rev),
    ]


def strict_grammar_errors(rev):
    if not SHA_RE.match(rev):
        return [f"grammar rev {rev!r} is not a 40-character commit SHA"]
    errors = []
    for name in GRAMMAR_FILES:
        if subprocess.run(
            ["git", "cat-file", "-e", f"{rev}:{GRAMMAR_SUBTREE}/{name}"],
            cwd=ROOT,
            check=False,
        ).returncode != 0:
            errors.append(f"grammar rev {rev} does not contain {GRAMMAR_SUBTREE}/{name}")
    if subprocess.run(
        ["git", "merge-base", "--is-ancestor", rev, "HEAD"],
        cwd=ROOT,
        check=False,
    ).returncode != 0:
        errors.append(f"grammar rev {rev} is not an ancestor of HEAD")
    if subprocess.run(
        ["git", "diff", "--quiet", rev, "HEAD", "--", GRAMMAR_SUBTREE],
        cwd=ROOT,
        check=False,
    ).returncode != 0:
        errors.append(f"grammar subtree differs between {rev} and HEAD")
    return errors


def check(args):
    release = load_release()
    version = release["version"]
    errors = []
    if not VERSION_RE.match(version):
        errors.append(f"extensions/shared/release.json: invalid version {version!r}")
    if args.expect_tag and args.expect_tag != f"extensions-v{version}":
        errors.append(f"tag {args.expect_tag!r} does not match extensions-v{version}")
    for label, actual in version_locations():
        if actual != version:
            errors.append(f"{label}: version {actual!r} != {version!r}")
    for label, actual, expected in grammar_locations(release):
        if actual != expected:
            errors.append(f"{label}: {actual!r} != {expected!r}")
    if args.strict_grammar:
        errors.extend(strict_grammar_errors(release["grammar"]["rev"]))
    if errors:
        for error in errors:
            print(f"error: {error}", file=sys.stderr)
        return 1
    print(f"extension version {version} and grammar pin {release['grammar']['rev']} are consistent")
    return 0


def prepare(args):
    version = args.version
    if not VERSION_RE.match(version):
        raise SystemExit(f"invalid version {version!r}; expected X.Y.Z")
    dirty = git("status", "--porcelain", "--", GRAMMAR_SUBTREE, capture=True)
    if dirty:
        raise SystemExit(
            "commit grammar changes before preparing a release:\n" + dirty
        )
    rev = args.grammar_rev or os.environ.get("GRAMMAR_REV") or git("rev-parse", "HEAD", capture=True)
    if not SHA_RE.match(rev):
        raise SystemExit(f"invalid grammar rev {rev!r}; expected a 40-character commit SHA")
    if subprocess.run(
        ["git", "cat-file", "-e", f"{rev}:{GRAMMAR_SUBTREE}/grammar.js"],
        cwd=ROOT,
        check=False,
    ).returncode != 0:
        raise SystemExit(f"grammar rev {rev} does not contain {GRAMMAR_SUBTREE}/grammar.js")

    release = load_release()
    release["version"] = version
    release["grammar"]["rev"] = rev
    repository = release["grammar"]["repository"]

    release_text = json.dumps(release, indent=2) + "\n"
    extension_toml = replace_once(
        read("extensions/zed/extension.toml"),
        r'^version = ".*"$',
        f'version = "{version}"',
        "extensions/zed/extension.toml version",
    )
    extension_toml = replace_all(
        extension_toml,
        r'^repository = ".*"$',
        f'repository = "{repository}"',
        "extensions/zed/extension.toml repository",
    )
    extension_toml = replace_once(
        extension_toml,
        r'^rev = ".*"$',
        f'rev = "{rev}"',
        "extensions/zed/extension.toml rev",
    )
    cargo_toml = replace_once(
        read("extensions/zed/Cargo.toml"),
        r'^version = ".*"$',
        f'version = "{version}"',
        "extensions/zed/Cargo.toml version",
    )
    cargo_lock = replace_once(
        read("extensions/zed/Cargo.lock"),
        r'(^name = "neutron-zed"\nversion = ")[^"]*(")$',
        rf"\g<1>{version}\g<2>",
        "extensions/zed/Cargo.lock neutron-zed version",
    )
    emacs = replace_once(
        read("extensions/emacs/neutron-mode.el"),
        r"^;; Version: .*$",
        f";; Version: {version}",
        "extensions/emacs/neutron-mode.el version",
    )
    tree_sitter_manifest = replace_once(
        read("extensions/shared/tree-sitter-neutron/tree-sitter.json"),
        r'^    "version": "[^"]*",$',
        f'    "version": "{version}",',
        "extensions/shared/tree-sitter-neutron/tree-sitter.json version",
    )

    changed = []
    for prefix in ("extensions/vscode", "extensions/shared/tree-sitter-neutron"):
        current = json.loads(read(f"{prefix}/package.json"))["version"]
        if current == version:
            continue
        try:
            subprocess.run(
                ["npm", "--prefix", prefix, "version", version, "--no-git-tag-version", "--silent"],
                cwd=ROOT,
                check=True,
            )
        except FileNotFoundError:
            raise SystemExit("npm is required to update package.json and package-lock.json") from None
        except subprocess.CalledProcessError as error:
            raise SystemExit(f"npm version failed for {prefix}: {error}") from error
        changed.extend([f"{prefix}/package.json", f"{prefix}/package-lock.json"])

    for path, content in (
        ("extensions/shared/release.json", release_text),
        ("extensions/zed/extension.toml", extension_toml),
        ("extensions/zed/Cargo.toml", cargo_toml),
        ("extensions/zed/Cargo.lock", cargo_lock),
        ("extensions/emacs/neutron-mode.el", emacs),
        ("extensions/shared/tree-sitter-neutron/tree-sitter.json", tree_sitter_manifest),
    ):
        if read(path) != content:
            write(path, content)
            changed.append(path)

    if not args.no_generate:
        subprocess.run(
            [sys.executable, "-B", "extensions/shared/generate.py"],
            cwd=ROOT,
            check=True,
        )

    print("updated: " + (", ".join(changed) if changed else "no file changes"))
    print(
        "\nNext:\n"
        "  git add extensions\n"
        f'  git commit -m "chore(extensions): release {version}"\n'
        f'  git tag -a extensions-v{version} -m "Neutron editor extensions {version}"\n'
        "  git push origin HEAD\n"
        f"  git push origin extensions-v{version}"
    )
    return 0


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)

    check_parser = subparsers.add_parser("check", help="validate versions and grammar pins")
    check_parser.add_argument("--strict-grammar", action="store_true")
    check_parser.add_argument("--expect-tag")
    check_parser.set_defaults(handler=check)

    prepare_parser = subparsers.add_parser("prepare", help="bump versions and pin the grammar")
    prepare_parser.add_argument("version")
    prepare_parser.add_argument("--grammar-rev")
    prepare_parser.add_argument("--no-generate", action="store_true")
    prepare_parser.set_defaults(handler=prepare)

    args = parser.parse_args()
    return args.handler(args)


if __name__ == "__main__":
    sys.exit(main())
