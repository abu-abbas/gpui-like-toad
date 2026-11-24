#!/usr/bin/env python3
"""Build the extension index `extensions.json` (schema 1) for one new
extension release, keeping the entries of the other extensions.

Usage: build-extension-index.py <meta.json> <version> <release-dir> <old-index|-> <out>

- <meta.json>: extensions/<id>/extension.json (id, name, description, kind,
  min_app, optional max_app).
- <release-dir>: the files of the new release, named
  kilat-ext-<id>-<version>-<os>-<arch>[.exe].
- <old-index>: the current, already verified index, or `-` for none.
The new revision is the old one plus one. The caller signs <out> with the
trusted comment `version:<revision>` (printed on stdout).
"""
import json
import os
import re
import sys


def main(meta_path, version, release_dir, old_path, out_path):
    meta = json.load(open(meta_path))
    ext_id = meta["id"]
    assert re.fullmatch(r"[a-z0-9][a-z0-9-]{0,39}", ext_id), f"bad id {ext_id!r}"
    assert re.fullmatch(r"\d+\.\d+\.\d+", version), f"bad version {version!r}"
    assert meta["kind"] in ("database", "tool", "action"), "kind must be database, tool or action"

    prefix = f"kilat-ext-{ext_id}-{version}-"
    files = {}
    for name in sorted(os.listdir(release_dir)):
        if not name.startswith(prefix) or name.endswith(".minisig"):
            continue
        platform = name[len(prefix):].removesuffix(".exe")
        assert re.fullmatch(r"(linux|macos|windows)-[a-z0-9_]+", platform), name
        files[platform] = name
    assert files, f"no files named {prefix}* in {release_dir}"

    entry = {
        "id": ext_id,
        "name": meta["name"],
        "description": meta.get("description", ""),
        "kind": meta["kind"],
        "version": version,
        "min_app": meta["min_app"],
        "tag": f"ext-{ext_id}-{version}",
        "files": files,
    }
    if meta.get("max_app"):
        entry["max_app"] = meta["max_app"]

    revision, others = 0, []
    if old_path != "-":
        old = json.load(open(old_path))
        assert old.get("schema") == 1, "old index has another schema"
        revision = int(old["revision"])
        others = [e for e in old.get("extensions", []) if e.get("id") != ext_id]
    index = {
        "schema": 1,
        "revision": revision + 1,
        "extensions": sorted(others + [entry], key=lambda e: e["id"]),
    }
    with open(out_path, "w") as f:
        json.dump(index, f, indent=2)
        f.write("\n")
    print(index["revision"])


if __name__ == "__main__":
    if len(sys.argv) != 6:
        sys.exit(__doc__)
    main(*sys.argv[1:])
