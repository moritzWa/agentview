"""Temporary display/mic setup ONLY inside the dedicated lume test VM."""

import argparse
import base64
import ctypes as c
import json
import re
import sqlite3
import subprocess
from pathlib import Path


def display(width, height):
    g = c.CDLL("/System/Library/Frameworks/CoreGraphics.framework/CoreGraphics")
    f = c.CDLL("/System/Library/Frameworks/CoreFoundation.framework/CoreFoundation")
    g.CGMainDisplayID.restype = c.c_uint32
    d = g.CGMainDisplayID()
    g.CGDisplayCopyAllDisplayModes.argtypes = [c.c_uint32, c.c_void_p]
    g.CGDisplayCopyAllDisplayModes.restype = c.c_void_p
    f.CFArrayGetCount.argtypes = [c.c_void_p]
    f.CFArrayGetCount.restype = c.c_long
    f.CFArrayGetValueAtIndex.argtypes = [c.c_void_p, c.c_long]
    f.CFArrayGetValueAtIndex.restype = c.c_void_p
    for name in ["CGDisplayModeGetWidth", "CGDisplayModeGetHeight"]:
        getattr(g, name).argtypes = [c.c_void_p]
        getattr(g, name).restype = c.c_size_t
    g.CGBeginDisplayConfiguration.argtypes = [c.POINTER(c.c_void_p)]
    g.CGConfigureDisplayWithDisplayMode.argtypes = [
        c.c_void_p,
        c.c_uint32,
        c.c_void_p,
        c.c_void_p,
    ]
    g.CGCompleteDisplayConfiguration.argtypes = [c.c_void_p, c.c_int]
    modes = g.CGDisplayCopyAllDisplayModes(d, None)
    for i in range(f.CFArrayGetCount(modes)):
        mode = f.CFArrayGetValueAtIndex(modes, i)
        if (g.CGDisplayModeGetWidth(mode), g.CGDisplayModeGetHeight(mode)) == (
            width,
            height,
        ):
            config = c.c_void_p()
            assert g.CGBeginDisplayConfiguration(c.byref(config)) == 0
            assert g.CGConfigureDisplayWithDisplayMode(config, d, mode, None) == 0
            assert g.CGCompleteDisplayConfiguration(config, 1) == 0
            return
    raise RuntimeError("Requested display mode not available")


def main():
    assert Path.home().name == "lume", "Never run this against the host account"
    ap = argparse.ArgumentParser()
    ap.add_argument("action", choices=["setup", "restore"])
    args = ap.parse_args()
    state = Path.home() / "agentview-film-vm-state.json"
    db = Path.home() / "Library/Application Support/com.apple.TCC/TCC.db"
    connection = sqlite3.connect(str(db))
    where = "service='kTCCServiceMicrophone' and client='com.openai.codex'"
    if args.action == "setup":
        assert not state.exists(), "Restore previous temporary capture setup first"
        columns = [r[1] for r in connection.execute("pragma table_info(access)")]
        rows = connection.execute("select * from access where " + where).fetchall()
        encode = lambda value: (
            {"blob": base64.b64encode(value).decode()}
            if isinstance(value, bytes)
            else value
        )
        state.write_text(
            json.dumps(
                {
                    "columns": columns,
                    "rows": [[encode(v) for v in row] for row in rows],
                },
                indent=2,
            )
        )
        result = subprocess.run(
            ["codesign", "-dr", "-", "/Applications/ChatGPT.app"],
            capture_output=True,
            text=True,
            check=True,
        )
        requirement = re.search(
            r"designated => (.*)", result.stdout + result.stderr
        ).group(1)
        cert = Path.home() / "agentview-film-capture.csreq"
        subprocess.run(
            ["csreq", "-r-", "-b", str(cert)], input=requirement.encode(), check=True
        )
        connection.execute(
            "insert or replace into access (service,client,client_type,auth_value,auth_reason,auth_version,csreq,flags) values (?,?,?,?,?,?,?,?)",
            (
                "kTCCServiceMicrophone",
                "com.openai.codex",
                0,
                2,
                4,
                1,
                cert.read_bytes(),
                0,
            ),
        )
        connection.commit()
        cert.unlink()
        display(3840, 2160)
    else:
        old = json.loads(state.read_text())
        connection.execute("delete from access where " + where)
        for row in old["rows"]:
            values = [
                base64.b64decode(v["blob"])
                if isinstance(v, dict) and "blob" in v
                else v
                for v in row
            ]
            connection.execute(
                "insert into access ("
                + ",".join(old["columns"])
                + ") values ("
                + ",".join("?" for _ in values)
                + ")",
                values,
            )
        connection.commit()
        display(1024, 768)
        state.unlink()
    subprocess.run(["killall", "tccd"], check=False)
    print(
        "Temporary VM capture setup applied."
        if args.action == "setup"
        else "VM microphone permission and1024x768 display restored."
    )


if __name__ == "__main__":
    main()
