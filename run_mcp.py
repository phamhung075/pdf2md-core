#!/usr/bin/env python3
"""Root CLI entrypoint for running the Model Context Protocol (MCP) server.

Usage:
  python run_mcp.py              # stdio transport (default; for Claude Desktop, Cursor, Antigravity)
  python run_mcp.py sse          # SSE transport on 127.0.0.1:3985
  python run_mcp.py sse 0.0.0.0 3985
"""
import sys
from src.interfaces.mcp.server import run_mcp_server

if __name__ == "__main__":
    transport = sys.argv[1].lower() if len(sys.argv) > 1 else "stdio"
    host = sys.argv[2] if len(sys.argv) > 2 else "127.0.0.1"
    port = int(sys.argv[3]) if len(sys.argv) > 3 else 3985
    run_mcp_server(transport=transport, host=host, port=port)
