#!/usr/bin/env python3
"""Create a zip archive from a directory, skipping Python cache files."""

import sys
import zipfile
from pathlib import Path


def main():
    if len(sys.argv) != 3:
        raise SystemExit("usage: zip-directory.py ARCHIVE DIRECTORY")
    archive_path = Path(sys.argv[1])
    source = Path(sys.argv[2])
    with zipfile.ZipFile(archive_path, "w", zipfile.ZIP_DEFLATED) as archive:
        for path in sorted(source.rglob("*")):
            if not path.is_file():
                continue
            relative = path.relative_to(source)
            if "__pycache__" in relative.parts or path.suffix == ".pyc":
                continue
            archive.write(path, relative.as_posix())


if __name__ == "__main__":
    main()
