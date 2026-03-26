#!/bin/sh
# Wrapper to run Python MCP servers with proper unbuffered I/O
exec python3 -u "$@"
