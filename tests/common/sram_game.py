# SPDX-License-Identifier: BSD-3-Clause
import json
import sys
from pathlib import Path


def run_sram_game_fixture() -> None:
    content = Path(sys.argv[-1])
    root = content.resolve().parents[4]
    assert sys.argv[sys.argv.index("--sram-mode") + 1] == "load-save"
    config = dict(
        parts for line in (Path.cwd() / "retroarch.cfg").read_text().splitlines()
        if len(parts := line.split(" = ", 1)) == 2
    )
    directory = Path(config["savefile_directory"].strip("\""))
    assert directory == root / "games/saves"
    states = root / "games/states"
    assert config["savestate_directory"].strip("\"") == f"{states}"
    sram = directory / f"{content.stem}.srm"
    rtc = directory / f"{content.stem}.rtc"
    old_sram = sram.read_bytes() if sram.exists() else b""
    old_rtc = rtc.read_bytes() if rtc.exists() else b""

    with (root / "save-trace").open("a") as trace_file:
        trace_file.write(json.dumps({"sram": old_sram.decode(), "rtc": old_rtc.decode()}) + "\n")

    sram.write_bytes(old_sram + b"synthetic progress;")
    rtc.write_bytes(old_rtc + b"synthetic clock;")


run_sram_game_fixture()
