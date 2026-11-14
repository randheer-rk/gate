#!/bin/sh
set -eu
project_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
exec cargo install --locked --path "$project_dir" --root "${GATE_INSTALL_ROOT:-$HOME/.local}"
