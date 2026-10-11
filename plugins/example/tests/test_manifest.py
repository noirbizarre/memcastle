"""Generated-project metadata checks, independent of the module implementations."""

from pathlib import Path
import tomllib
import unittest


ROOT = Path(__file__).resolve().parents[1]


class PluginManifestTests(unittest.TestCase):
    def test_module_ids_are_unique_and_stable(self):
        plugin = tomllib.loads((ROOT / "plugin.toml").read_text())
        names = [module["id"] for module in plugin.get("modules", [])]
        self.assertEqual(len(names), len(set(names)))
        self.assertTrue(plugin["plugin"]["id"])

    def test_declared_module_manifests_exist(self):
        plugin = tomllib.loads((ROOT / "plugin.toml").read_text())
        for module in plugin.get("modules", []):
            self.assertTrue((ROOT / module["manifest"]).is_file())


if __name__ == "__main__":
    unittest.main()
