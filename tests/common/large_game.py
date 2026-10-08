# SPDX-License-Identifier: BSD-3-Clause
import os
import sys
from pathlib import Path


def run_large_game_fixture() -> None:
    root = Path(sys.argv[-1]).resolve().parents[4]
    save = Path.cwd() / "states/large.state"
    save.write_bytes(b"s" * 131072)
    os.write(1, b"fixture stdout\n")
    os.write(2, b"fixture stderr\n")

    for descriptor, data in [(1, b"o" * 1048576), (2, b"e" * 1048576)]:

        while data:
            data = data[os.write(descriptor, data):]

    (root / "large-file-size").write_text(f"{save.stat().st_size}")


run_large_game_fixture()
