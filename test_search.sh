#!/bin/bash

# Build the project in debug mode
echo "Building inlyne..."
cargo build 2>&1

if [ $? -ne 0 ]; then
    echo "Build failed!"
    exit 1
fi

# Set up logging
export RUST_LOG=inlyne=debug

echo "Running inlyne with test document..."
echo "Press Ctrl+F to activate search"
echo "Type to search, 'n' or Enter for next match, Shift+n for previous match"
echo "Press Escape to cancel search"
echo ""

# Run inlyne with the test document
cargo run -- test_search/test.md