#!/usr/bin/env bash
# Are the four config failures Windows-only? Same tests, same source, Linux target dir.
cd /mnt/c/Users/orhay/ansible-lsp || exit 1
export CARGO_TARGET_DIR=/tmp/ansible-lsp-linux-target
export PATH="$HOME/.cargo/bin:$PATH"
cargo test -q -p ansible-core --lib config:: 2>&1 | tail -20
