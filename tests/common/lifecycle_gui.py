# SPDX-License-Identifier: BSD-3-Clause
# The Rust fixture prepends ipc.py; no imports depend on the launch directory.
import json
import os
import socket
import struct
import sys
import time
from pathlib import Path


def run_gui_fixture() -> None:

    with socket.socket(socket.AF_UNIX) as connection:
        connection.connect(sys.argv[sys.argv.index("--ipc-socket") + 1])
        client = RpcClient(connection)
        bootstrap = client.call("igui.bootstrap")["result"]
        root = Path(bootstrap["root"])
        mode = (root / "mode").read_text()

        with (root / "trace").open("a") as trace_file:
            trace_file.write(json.dumps({
                "event": "gui",
                "pid": os.getpid(),
                "bootstrap": bootstrap,
                "args": sys.argv[1:],
            }) + "\n")

        if mode == "gui-error":
            sys.exit(9)

        if mode == "oversized":
            connection.sendall(struct.pack("!I", 20000))
            time.sleep(60)

        if bootstrap["state"]["selected_hash"] is None:
            state = bootstrap["state"]
            state["selected_hash"] = "ab" * 32
            state["platform"] = "NES"
            (root / "gui-pid").write_text(f"{os.getpid()}")
            game = {"hash": "ab" * 32, "platform": "NES", "image": "NES/ab/Game ' one.nes"}
            ready = client.call("igui.prepare_launch", game)

            if mode == "handoff-error" and ready["status"] == "done":
                (root / "games/images" / game["image"]).unlink()
                ready = client.call("igui.handoff", {"state": state, "game": game})

            if ready["status"] == "error":
                (root / "preflight-error").write_text(ready["error"]["code"])
                (root / "preflight-detail").write_text(ready["error"]["message"])
                assert client.call("igui.closed")["status"] == "done"

            else:
                assert client.call("igui.handoff", {"state": state, "game": game})["status"] == "done"

        else:
            assert client.call("igui.closed")["status"] == "done"


run_gui_fixture()
