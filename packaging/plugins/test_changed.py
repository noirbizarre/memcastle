"""Provider-only changes must not fan out to every provider job."""

from importlib.util import module_from_spec, spec_from_file_location
from pathlib import Path
import unittest


spec = spec_from_file_location("changed", Path(__file__).with_name("changed.py"))
changed = module_from_spec(spec)
spec.loader.exec_module(changed)


class ChangeSelectionTests(unittest.TestCase):
    def test_provider_only_edit_runs_only_that_provider(self):
        self.assertEqual(changed.select(["plugins/openai/modules/chatgpt/src/lib.rs"], ["claude", "openai"]), ["openai"])

    def test_wit_or_host_contract_change_runs_every_provider(self):
        self.assertEqual(changed.select(["wit/memcastle-source.wit"], ["claude", "openai"]), ["claude", "openai"])
        self.assertEqual(changed.select(["src/plugin/manifest.rs"], ["claude", "openai"]), ["claude", "openai"])
        self.assertEqual(changed.select(["src/app/plugins.rs"], ["claude", "openai"]), ["claude", "openai"])


if __name__ == "__main__":
    unittest.main()
