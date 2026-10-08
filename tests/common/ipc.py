# SPDX-License-Identifier: BSD-3-Clause
import json
import socket
import struct
from typing import Any


class RpcClient:
    def __init__(self, connection: socket.socket) -> None:
        self.connection = connection
        self.request_id = 0

    def read_exact(self, size: int) -> bytes:
        data = b""

        while len(data) < size:
            part = self.connection.recv(size - len(data))

            if not part:
                raise RuntimeError("RPC peer gone")

            data += part

        return data

    def call(self, command: str, params: dict[str, Any] | None = None) -> dict[str, Any]:
        self.request_id += 1
        data = json.dumps({
            "kind": "request",
            "id": self.request_id,
            "command": command,
            "params": {} if params is None else params,
        }).encode()
        self.connection.sendall(struct.pack("!I", len(data)) + data)
        response_size = struct.unpack("!I", self.read_exact(4))[0]
        response = json.loads(self.read_exact(response_size))
        assert response["id"] == self.request_id
        return response
