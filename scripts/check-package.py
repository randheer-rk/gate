"""Assert that the Cargo source package contains its client integrations."""

import json
import sys
import tarfile

required = {
    "plugins/claude-code/.claude-plugin/plugin.json",
    "plugins/claude-code/.mcp.json",
    "plugins/codex/config.toml.snippet",
    "scripts/install.sh",
    "LICENSE",
}
assert len(sys.argv) > 1, "usage: check-package.py <crate> [<crate> ...]"
for archive in sys.argv[1:]:
    with tarfile.open(archive) as package:
        paths = {name.partition("/")[2]: name for name in package.getnames()}
        assert required <= paths.keys(), f"{archive} is missing: {required - paths.keys()}"
        for path in required:
            if path.endswith(".json"):
                json.load(package.extractfile(paths[path]))
print("Package includes the installer, both client integrations, and license")
