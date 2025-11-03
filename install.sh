#!/bin/bash
if [ -z "$bin" ]; then
    echo "bin var must be set (e.g., ~/bin)"
    exit 1
fi

echo "Building faucet..."
cargo build --release || exit 1
ln -sf "$(pwd)/target/release/faucet" "$bin/faucet"
echo "Linked $(realpath target/release/faucet) to $bin"
