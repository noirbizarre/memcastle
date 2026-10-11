#!/usr/bin/env python3
"""Select changed provider projects, not unrelated source packages or Core jobs."""

import json
import os
from pathlib import Path
import subprocess


def select(paths: list[str], providers: list[str]) -> list[str]:
    """Run all providers for a changed contract, otherwise only touched provider roots."""
    shared = any(path.startswith((
        "wit/", "src/plugin/", "src/distribution/plugin.rs", "src/domain/plugin",
        "src/app/plugins.rs", "src/api/plugins.rs", "src/store/plugins.rs", "database/schema/plugin",
        "src/config/mod.rs",
        "src/integration/", "packaging/plugins/",
        "src/mining/", "src/source/", "src/domain/source_package.rs",
        "src/app/source_packages.rs", "database/schema/source_package.surql",
        "tests/wasm_plugin.rs", ".github/workflows/plugins.yaml",
    )) for path in paths)
    return providers if shared else sorted({
        path.split("/")[1] for path in paths
        if path.startswith("plugins/") and path.split("/")[1] in providers
    })


def main() -> None:
    base = os.environ.get("BASE", "")
    head = os.environ.get("GITHUB_SHA", "HEAD")
    providers = sorted(path.name for path in Path("plugins").iterdir() if (path / "plugin.toml").is_file())
    if not base or set(base) == {"0"}:
        selected = providers
    else:
        paths = subprocess.run(
            ["git", "diff", "--name-only", base, head, "--"],
            check=True, capture_output=True, text=True,
        ).stdout.splitlines()
        selected = select(paths, providers)
    if output := os.environ.get("GITHUB_OUTPUT"):
        with open(output, "a", encoding="utf-8") as stream:
            stream.write(f"providers={json.dumps(selected, separators=(',', ':'))}\n")
    else:
        print(json.dumps(selected))


if __name__ == "__main__":
    main()
