"""MCP (Model Context Protocol) Server and Client implementation.

Implements the MCP protocol for tool communication:
- MCPServer: Exposes tools to agents via MCP
- MCPClient: Connects to external MCP servers
"""

from __future__ import annotations

import asyncio
import json
import logging
from dataclasses import dataclass
from typing import Any, Dict, List, Optional, AsyncIterator, TYPE_CHECKING

if TYPE_CHECKING:
    from xpressai.tools.registry import ToolRegistry

logger = logging.getLogger(__name__)


@dataclass
class MCPMessage:
    """An MCP protocol message."""

    jsonrpc: str = "2.0"
    id: Optional[int] = None
    method: Optional[str] = None
    params: Optional[Dict[str, Any]] = None
    result: Optional[Any] = None
    error: Optional[Dict[str, Any]] = None

    def to_json(self) -> str:
        """Serialize to JSON string."""
        data = {"jsonrpc": self.jsonrpc}
        if self.id is not None:
            data["id"] = self.id
        if self.method:
            data["method"] = self.method
        if self.params:
            data["params"] = self.params
        if self.result is not None:
            data["result"] = self.result
        if self.error:
            data["error"] = self.error
        return json.dumps(data)

    @classmethod
    def from_json(cls, data: str) -> MCPMessage:
        """Deserialize from JSON string."""
        parsed = json.loads(data)
        return cls(
            jsonrpc=parsed.get("jsonrpc", "2.0"),
            id=parsed.get("id"),
            method=parsed.get("method"),
            params=parsed.get("params"),
            result=parsed.get("result"),
            error=parsed.get("error"),
        )


class MCPServer:
    """MCP Server that exposes tools to agents.

    Implements the MCP protocol server-side, allowing agents to
    discover and call tools.
    """

    def __init__(self, registry: ToolRegistry, name: str = "xpressai"):
        self.registry = registry
        self.name = name
        self._running = False
        self._request_id = 0

    async def start(self) -> None:
        """Start the MCP server."""
        self._running = True
        logger.info(f"MCP Server '{self.name}' started")

    async def stop(self) -> None:
        """Stop the MCP server."""
        self._running = False
        logger.info(f"MCP Server '{self.name}' stopped")

    async def handle_message(self, message: MCPMessage) -> MCPMessage:
        """Handle an incoming MCP message.

        Args:
            message: The incoming message

        Returns:
            Response message
        """
        if message.method == "initialize":
            return await self._handle_initialize(message)
        elif message.method == "tools/list":
            return await self._handle_list_tools(message)
        elif message.method == "tools/call":
            return await self._handle_call_tool(message)
        else:
            return MCPMessage(
                id=message.id,
                error={"code": -32601, "message": f"Method not found: {message.method}"},
            )

    async def _handle_initialize(self, message: MCPMessage) -> MCPMessage:
        """Handle initialize request."""
        return MCPMessage(
            id=message.id,
            result={
                "protocolVersion": "2024-11-05",
                "serverInfo": {
                    "name": self.name,
                    "version": "0.1.0",
                },
                "capabilities": {
                    "tools": {},
                },
            },
        )

    async def _handle_list_tools(self, message: MCPMessage) -> MCPMessage:
        """Handle tools/list request."""
        tools = self.registry.get_tool_schemas()
        return MCPMessage(id=message.id, result={"tools": tools})

    async def _handle_call_tool(self, message: MCPMessage) -> MCPMessage:
        """Handle tools/call request."""
        params = message.params or {}
        tool_name = params.get("name")
        arguments = params.get("arguments", {})

        if not tool_name:
            return MCPMessage(id=message.id, error={"code": -32602, "message": "Missing tool name"})

        try:
            result = await self.registry.call_tool(tool_name, arguments)
            return MCPMessage(
                id=message.id, result={"content": [{"type": "text", "text": str(result)}]}
            )
        except Exception as e:
            logger.error(f"Tool call failed: {e}")
            return MCPMessage(id=message.id, error={"code": -32000, "message": str(e)})


class MCPClient:
    """MCP Client for connecting to external MCP servers.

    Connects to external MCP servers and makes their tools
    available to the registry.
    """

    def __init__(self, name: str):
        self.name = name
        self._connected = False
        self._process: Optional[asyncio.subprocess.Process] = None
        self._request_id = 0
        self._pending_requests: Dict[int, asyncio.Future] = {}

    async def connect_stdio(self, command: List[str]) -> None:
        """Connect to an MCP server via stdio.

        Args:
            command: Command to start the MCP server
        """
        self._process = await asyncio.create_subprocess_exec(
            *command,
            stdin=asyncio.subprocess.PIPE,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
        )
        self._connected = True

        # Start reading responses
        asyncio.create_task(self._read_responses())

        # Send initialize
        await self._send_request(
            "initialize",
            {
                "protocolVersion": "2024-11-05",
                "clientInfo": {
                    "name": "xpressai",
                    "version": "0.1.0",
                },
                "capabilities": {},
            },
        )

        logger.info(f"Connected to MCP server: {self.name}")

    async def disconnect(self) -> None:
        """Disconnect from the MCP server."""
        if self._process:
            self._process.terminate()
            await self._process.wait()
            self._process = None
        self._connected = False
        logger.info(f"Disconnected from MCP server: {self.name}")

    async def _read_responses(self) -> None:
        """Read responses from the MCP server."""
        if not self._process or not self._process.stdout:
            return

        while self._connected:
            try:
                line = await self._process.stdout.readline()
                if not line:
                    break

                message = MCPMessage.from_json(line.decode())

                if message.id is not None and message.id in self._pending_requests:
                    future = self._pending_requests.pop(message.id)
                    if message.error:
                        future.set_exception(
                            Exception(message.error.get("message", "Unknown error"))
                        )
                    else:
                        future.set_result(message.result)
            except Exception as e:
                logger.error(f"Error reading MCP response: {e}")

    async def _send_request(self, method: str, params: Optional[Dict[str, Any]] = None) -> Any:
        """Send a request to the MCP server.

        Args:
            method: The method to call
            params: Optional parameters

        Returns:
            The result of the request
        """
        if not self._process or not self._process.stdin:
            raise RuntimeError("Not connected to MCP server")

        self._request_id += 1
        request_id = self._request_id

        message = MCPMessage(
            id=request_id,
            method=method,
            params=params,
        )

        future: asyncio.Future = asyncio.Future()
        self._pending_requests[request_id] = future

        self._process.stdin.write((message.to_json() + "\n").encode())
        await self._process.stdin.drain()

        return await future

    async def list_tools(self) -> List[Dict[str, Any]]:
        """List available tools from the MCP server.

        Returns:
            List of tool definitions
        """
        result = await self._send_request("tools/list")
        return result.get("tools", [])

    async def call_tool(self, name: str, arguments: Dict[str, Any]) -> Any:
        """Call a tool on the MCP server.

        Args:
            name: Name of the tool
            arguments: Arguments to pass

        Returns:
            The result of the tool call
        """
        result = await self._send_request(
            "tools/call",
            {
                "name": name,
                "arguments": arguments,
            },
        )
        return result
