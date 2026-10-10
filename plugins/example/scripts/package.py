#!/usr/bin/env python3
"""Build this repository's modules and package the reviewed plugin release.

Run deliberately by a plugin author, never by GitTPL or by the MemCastle daemon.
The only archive entries are the root manifest and its declared module artifacts.
"""

import gzip
import hashlib
import io
from pathlib import Path
import shutil
import subprocess
import tarfile
import tomllib

ROOT = Path(__file__).resolve().parents[1]
MANIFEST = ROOT / "plugin.toml"
MAX_FILES = 1024
MAX_SIZE = 256 * 1024 * 1024


def safe_file(name: str) -> Path:
    parts = Path(name).parts
    if not parts or "\\" in name or any(part in ("", ".", "..") for part in parts) or Path(name).is_absolute():
        raise ValueError(f"unsafe module path {name!r}")
    path = ROOT
    for part in parts:
        path = path / part
        if path.is_symlink():
            raise ValueError(f"module path {name!r} traverses a symlink")
    if not path.is_file():
        raise ValueError(f"missing module file {name!r}")
    return path


def add(files: dict[str, bytes], name: str) -> None:
    files[name] = safe_file(name).read_bytes()
    if len(files) > MAX_FILES or sum(map(len, files.values())) > MAX_SIZE:
        raise ValueError("plugin artifacts exceed the file count or size limit")


def directory(files: dict[str, bytes], name: str) -> None:
    path = ROOT / name
    if path.is_symlink() or not path.exists():
        raise ValueError(f"integration asset {name!r} is missing or a symlink")
    if path.is_file():
        add(files, name)
    else:
        for entry in sorted(path.rglob("*")):
            if entry.is_symlink():
                raise ValueError(f"integration asset {entry} is a symlink")
            if entry.is_file():
                add(files, entry.relative_to(ROOT).as_posix())


def archive(files: dict[str, bytes], destination: Path) -> None:
    destination.parent.mkdir(exist_ok=True)
    # Fix both tar and gzip metadata so re-packaging the same bytes gives the same digest.
    with destination.open("wb") as output, gzip.GzipFile(fileobj=output, mode="wb", filename="", mtime=0) as compressed:
        with tarfile.open(fileobj=compressed, mode="w") as bundle:
            for name, data in sorted(files.items()):
                entry = tarfile.TarInfo(name)
                entry.size = len(data)
                entry.mode = 0o644
                entry.mtime = 0
                bundle.addfile(entry, io.BytesIO(data))


def main() -> None:
    text = MANIFEST.read_text()
    manifest = tomllib.loads(text)
    files: dict[str, bytes] = {}
    sections = text.split("\n[[modules]]")
    if len(sections) != len(manifest.get("modules", [])) + 1:
        raise ValueError("plugin.toml module sections must match its declared modules")
    for index, module in enumerate(manifest.get("modules", []), start=1):
        name, kind = module["id"], module["type"]
        module_dir = ROOT / "modules" / name
        if kind == "source":
            if not (module_dir / "Cargo.lock").is_file():
                subprocess.run(["cargo", "generate-lockfile"], cwd=module_dir, check=True)
            subprocess.run(["cargo", "build", "--locked", "--release", "--target", "wasm32-wasip2"], cwd=module_dir, check=True)
            built = module_dir / "target/wasm32-wasip2/release" / f"{name.replace('-', '_')}.wasm"
            if not built.is_file():
                raise ValueError(f"source {name!r} built no component at {built}")
            shutil.copyfile(built, safe_parent(module["entry"]))
            # The conformance runner uses each module's own `dist/source.wasm`; this is build output,
            # not another copy inside the plugin archive.
            test_component = module_dir / "dist/source.wasm"
            test_component.parent.mkdir(exist_ok=True)
            shutil.copyfile(built, test_component)
        elif kind == "integration":
            with safe_file(module["manifest"]).open("rb") as source:
                integration = tomllib.load(source)
            prefix = f"modules/{name}"
            for asset in integration.get("assets", []):
                directory(files, f"{prefix}/{asset['from']}")
            for skill in integration.get("skills", []):
                directory(files, (prefix + "/" if skill.get("local", False) else "") + f"skills/{skill['name']}")
        else:
            raise ValueError(f"no packaging contract for module kind {kind!r}")
        add(files, module["manifest"])
        add(files, module["entry"])
        section = sections[index]
        for key, path in (("manifest_sha256", module["manifest"]), ("sha256", module["entry"])):
            before = f'{key} = "{module[key]}"'
            if section.count(before) != 1:
                raise ValueError(f"module {name!r} has an ambiguous {key}")
            section = section.replace(before, f'{key} = "{hashlib.sha256(files[path]).hexdigest()}"', 1)
        sections[index] = section
    text = "\n[[modules]]".join(sections)
    MANIFEST.write_text(text)
    files["plugin.toml"] = text.encode()
    release = manifest["plugin"]
    destination = ROOT / "dist" / f"{release['id']}-{release['version']}.tar.gz"
    archive(files, destination)
    print(destination)


def safe_parent(name: str) -> Path:
    """The sole file the build writes: source.wasm in the module's own directory."""
    parts = Path(name).parts
    if len(parts) != 3 or parts[0] != "modules" or parts[2] != "source.wasm":
        raise ValueError(f"unsafe source component destination {name!r}")
    parent = ROOT
    for part in parts[:-1]:
        parent = parent / part
        if parent.is_symlink() or not parent.is_dir():
            raise ValueError(f"unsafe source component destination {name!r}")
    return parent / "source.wasm"


if __name__ == "__main__":
    main()
