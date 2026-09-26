#!/bin/sh
# Builds the on-chain binary, then runs every test against it and the Percolator binary.
set -e
cd "$(dirname "$0")"
cargo build-sbf --tools-version v1.53 --sbf-out-dir target/deploy >/dev/null 2>&1 || cargo build-sbf --tools-version v1.53 --sbf-out-dir target/deploy
cp keys/percolator_vault-keypair.json target/deploy/percolator_vault-keypair.json
cargo test "$@"
