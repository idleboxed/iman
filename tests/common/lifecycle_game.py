# SPDX-License-Identifier: BSD-3-Clause
import json
import os
import sys
import time
from pathlib import Path


def run_game_fixture() -> None:
    content = Path(sys.argv[-1])
    assert content.is_symlink() and content.stem == "ab" * 32
    root = content.resolve().parents[4]

    for descriptor in Path("/proc/self/fd").iterdir():

        try:
            assert descriptor.resolve() != root / "manager-resource", "inherited manager resource"

        except FileNotFoundError:
            pass

    try:
        os.kill(int((root / "gui-pid").read_text()), 0)

    except ProcessLookupError:
        pass

    else:
        raise AssertionError("GUI still alive during game")

    runtime = Path.cwd()
    expected = (root / "runtime-parent").read_text() if (root / "runtime-parent").exists() else "/dev/shm"
    assert runtime.parent == Path(expected) and runtime.name.startswith("iman-")
    assert "--sram-mode" in sys.argv and "load-save" in sys.argv
    assert os.environ["HOME"] == f"{runtime}"
    (runtime / "states/temporary.state").write_bytes(b"synthetic save")
    remaps = {f"{path.relative_to(runtime)}": path.read_text() for path in (runtime / "remaps").rglob("*.rmp")}
    event = {
        "event": "game",
        "pid": os.getpid(),
        "runtime": f"{runtime}",
        "config": (runtime / "retroarch.cfg").read_text(),
        "remaps": remaps,
        "args": sys.argv[1:],
    }

    with (root / "trace").open("a") as trace_file:
        trace_file.write(json.dumps(event) + "\n")

    mode = (root / "mode").read_text()

    if mode == "cancel":
        time.sleep(60)

    sys.exit(7 if mode == "game-error" else 0)


run_game_fixture()
