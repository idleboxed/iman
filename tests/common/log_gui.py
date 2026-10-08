# SPDX-License-Identifier: BSD-3-Clause
# The Rust fixture prepends ipc.py; no imports depend on the launch directory.
import os
import signal
import socket
import stat
import sys


def run_log_gui_fixture() -> None:
    signal.signal(signal.SIGXFSZ, signal.SIG_DFL)
    assert stat.S_ISSOCK(os.fstat(1).st_mode)
    assert stat.S_ISSOCK(os.fstat(2).st_mode)

    for descriptor, data in [(1, b"gui-head\n" + b"o" * 262144), (2, b"e" * 262144 + b"\ngui-tail\n")]:

        while data:
            data = data[os.write(descriptor, data):]

    # An explicit preference write must not inherit the 64 KiB logging limit.
    with open(sys.argv[2], "wb") as preferences_file:
        preferences_file.write(b"p" * 131072)

    with socket.socket(socket.AF_UNIX) as connection:
        connection.connect(sys.argv[1])
        client = RpcClient(connection)

        for command in ["igui.bootstrap", "igui.closed"]:
            response = client.call(command)
            assert response["status"] == "done", response


run_log_gui_fixture()
