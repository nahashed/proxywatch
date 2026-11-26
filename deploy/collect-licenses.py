#!/usr/bin/env python3
"""Collect upstream notices for the two release targets using locked, offline Cargo."""

import argparse
import hashlib
import json
import re
import subprocess
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
TARGETS = ("aarch64-unknown-linux-gnu", "x86_64-unknown-linux-gnu")
LEGAL_NAME = re.compile(
    r"^(?:licen[cs]e|copying|copyright|notice|authors|unlicense|third[-_]party)"
    r"(?:[._-]|$)", re.IGNORECASE
)


def dependencies(vendor):
    cargo = ["cargo", "--offline", "--locked"]
    if vendor:
        cargo += ["--config", 'source.crates-io.replace-with="vendored-sources"',
                  "--config", "source.vendored-sources.directory=" + json.dumps(str(vendor.resolve()))]
    packages = {}
    for target in TARGETS:
        def run(*args):
            return subprocess.check_output([*cargo, *args], cwd=ROOT, text=True)

        # Metadata also resolves features of dev-dependencies. Let Cargo tree
        # select the graph actually reached by normal edges for this target.
        metadata = json.loads(run("metadata", "--format-version", "1", "--filter-platform", target))
        names = {}
        for package in metadata["packages"]:
            key = (package["name"], "v" + package["version"])
            if key in names:
                raise ValueError(f"ambiguous package sources for {key}")
            names[key] = package
        tree = run("tree", "--target", target, "--edges", "normal", "--prefix", "none",
                   "--format", "{p}", "--color", "never")
        for line in sorted(set(tree.splitlines())):
            package = names[tuple(line.split()[:2])]
            if package["id"] == metadata["resolve"]["root"]:
                continue
            record = packages.setdefault(package["id"], (package, set()))
            record[1].add(target)
    return sorted(packages.values(), key=lambda item: (item[0]["name"], item[0]["version"]))


def source_notices(source, folders):
    # Preserve upstream grants and attribution from individual source files.
    comment = re.compile(r"^\s*(?://|/\*|\*|(?:#|@|;)(?:\s|$))")
    attribution = re.compile(r"copyright|permission is hereby granted|written by|"
                             r"original author|derived from", re.IGNORECASE)
    blocks = {}
    for folder in folders:
        for path in sorted((source / folder).rglob("*")):
            if path.suffix not in {".rs", ".c", ".h", ".inl", ".S", ".asm", ".pl"}:
                continue
            lines = path.read_text(encoding="utf-8").splitlines()
            block = []
            for line in lines + [""]:
                if comment.match(line):
                    block.append(line)
                elif block:
                    text = "\n".join(block)
                    if attribution.search(text):
                        blocks.setdefault(text, []).append(str(path.relative_to(source)))
                    block = []
    if not blocks:
        raise ValueError(f"source notices missing from {source.name}")
    return "\n\n".join("\n".join(paths) + "\n\n" + text for text, paths in blocks.items()) + "\n"


def notices(package):
    source = Path(package["manifest_path"]).parent
    files = {str(p.relative_to(source)): p.read_bytes() for p in source.rglob("*")
             if p.is_file() and (LEGAL_NAME.match(p.name)
                                 or p.parent.name.lower() in ("licenses", "licences"))}
    if package["license_file"]:
        path = Path(package["license_file"])
        files[str(path)] = (source / path).read_bytes()
    if not files:
        raise ValueError(f"upstream license files missing for {package['id']}")
    if package["name"] == "ring":
        # ring's LICENSE explicitly refers to grants in its source headers.
        folders = ("crypto", "include", "pregenerated", "src", "third_party")
        files["SOURCE-NOTICES.txt"] = source_notices(source, folders).encode()
    if package["name"] == "rustls-webpki":
        files["SOURCE-NOTICES.txt"] = source_notices(source, ["src"]).encode()
    if package["name"] in ("webpki-roots", "unsafe-libyaml"):
        # The readme identifies the source of the derived data or library.
        files["README.md"] = (source / "README.md").read_bytes()
    if package["name"] == "libsqlite3-sys":
        # SQLite ships its public-domain dedication in the amalgamation.
        amalgamation = (source / "sqlite3/sqlite3.c").read_bytes()
        start = amalgamation.index(b"** The author disclaims copyright")
        last_line = amalgamation.index(b"May you share freely", start)
        end = amalgamation.index(b"\n", last_line)
        files["sqlite3/DEDICATION.txt"] = amalgamation[start:end] + b"\n"
    return files


def sha256(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def checksums(paths, root):
    return "".join(f"{sha256(root / p)}  {p}\n" for p in sorted(paths))


def collect(output, vendor):
    if output.exists() or output.is_symlink():
        raise ValueError("output already exists")
    with tempfile.TemporaryDirectory(dir=output.parent) as temporary:
        stage = Path(temporary) / "notices"
        stage.mkdir()
        index = []
        for package, targets in dependencies(vendor):
            directory = package["name"] + "-" + package["version"]
            files = notices(package)
            for name, data in sorted(files.items()):
                if Path(name).is_absolute() or ".." in Path(name).parts or not data:
                    raise ValueError(f"invalid notice file in {directory}")
                path = stage / directory / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(data)
            index.append({"name": package["name"], "version": package["version"],
                          "license": package["license"], "source": package["source"],
                          "targets": sorted(targets), "directory": directory, "files": sorted(files)})
        (stage / "index.json").write_text(json.dumps(index, indent=2) + "\n", encoding="utf-8")
        (stage / "cargo-inputs.sha256").write_text(checksums(["Cargo.toml", "Cargo.lock"], ROOT))
        paths = [p.relative_to(stage) for p in stage.rglob("*") if p.is_file()]
        (stage / "SHA256SUMS").write_text(checksums(paths, stage))
        stage.rename(output)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", type=Path, help="new directory for THIRD-PARTY-LICENSES")
    parser.add_argument("--vendor", type=Path, help="crate sources prepared by cargo vendor")
    args = parser.parse_args()
    collect(args.output, args.vendor)
