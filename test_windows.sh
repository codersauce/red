#!/bin/bash
# Test script for window splits

echo "Testing window splits in Red editor"
echo "Commands to test:"
echo "  :split or :sp - horizontal split"
echo "  :vsplit or :vs - vertical split"  
echo "  Ctrl-w s - horizontal split"
echo "  Ctrl-w v - vertical split"
echo "  Ctrl-w w - next window"
echo "  Ctrl-w W - previous window"
echo ""
echo "Press any key to start..."
read -n 1

# Run the editor with test config
RED_CONFIG_FILE=test_config.toml cargo run -- test_windows.txt