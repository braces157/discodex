import ctypes
import ctypes.wintypes as wintypes
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile


class Coord(ctypes.Structure):
    _fields_ = [("x", ctypes.c_short), ("y", ctypes.c_short)]


class SmallRect(ctypes.Structure):
    _fields_ = [
        ("left", ctypes.c_short),
        ("top", ctypes.c_short),
        ("right", ctypes.c_short),
        ("bottom", ctypes.c_short),
    ]


class BufferInfo(ctypes.Structure):
    _fields_ = [
        ("size", Coord),
        ("cursor", Coord),
        ("attributes", wintypes.WORD),
        ("window", SmallRect),
        ("maximum", Coord),
    ]


def probe_parent_console(executable, result_path):
    kernel = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel.GetStdHandle.argtypes = [wintypes.DWORD]
    kernel.GetStdHandle.restype = wintypes.HANDLE
    kernel.GetConsoleScreenBufferInfo.argtypes = [
        wintypes.HANDLE, ctypes.POINTER(BufferInfo)
    ]
    kernel.GetConsoleScreenBufferInfo.restype = wintypes.BOOL
    kernel.ReadConsoleOutputCharacterW.argtypes = [
        wintypes.HANDLE, wintypes.LPWSTR, wintypes.DWORD,
        Coord, ctypes.POINTER(wintypes.DWORD),
    ]
    kernel.ReadConsoleOutputCharacterW.restype = wintypes.BOOL
    handle = kernel.GetStdHandle(-11 & 0xFFFFFFFF)
    info = BufferInfo()
    assert kernel.GetConsoleScreenBufferInfo(handle, ctypes.byref(info)), (
        ctypes.get_last_error()
    )
    print("DISCODEX_PARENT_CONSOLE_PROBE", flush=True)
    process = subprocess.run([executable, "--diagnostics"], timeout=60)
    assert kernel.GetConsoleScreenBufferInfo(handle, ctypes.byref(info)), (
        ctypes.get_last_error()
    )
    size = info.size.x * info.size.y
    buffer = ctypes.create_unicode_buffer(size + 1)
    read = wintypes.DWORD()
    assert kernel.ReadConsoleOutputCharacterW(
        handle, buffer, size, Coord(0, 0), ctypes.byref(read)
    ), ctypes.get_last_error()
    text = buffer.value
    result = {
        "parent_console_works": "DISCODEX_PARENT_CONSOLE_PROBE" in text,
        "report_visible": "Discodex diagnostics" in text,
        "exit_code": process.returncode,
    }
    Path(result_path).write_text(json.dumps(result), encoding="utf-8")


def verify(executable):
    executable = Path(executable).resolve(strict=True)
    with tempfile.TemporaryDirectory(dir=executable.parent) as scratch:
        scratch = Path(scratch)
        config_dir = scratch / "Discodex"
        config_dir.mkdir()
        (config_dir / "config.toml").write_text(
            "enabled = false\nforeground_detection = false\n"
            "[privacy]\nshare_reasoning = false\n"
            "show_project_names = false\nshow_file_names = false\n",
            encoding="utf-8",
        )
        env = os.environ.copy()
        env["APPDATA"] = str(scratch)
        result_path = scratch / "console-result.json"
        startup = subprocess.STARTUPINFO()
        startup.dwFlags |= subprocess.STARTF_USESHOWWINDOW
        startup.wShowWindow = 0
        subprocess.run(
            [sys.executable, str(Path(__file__).resolve()), "--parent-console",
             str(executable), str(result_path)],
            env=env, startupinfo=startup,
            creationflags=subprocess.CREATE_NEW_CONSOLE,
            timeout=75, check=True,
        )
        result = json.loads(result_path.read_text(encoding="utf-8"))
        assert result["parent_console_works"], result
        assert result["exit_code"] == 0, result
        assert result["report_visible"], result
        redirected = subprocess.run(
            [str(executable), "--diagnostics"], env=env,
            capture_output=True, text=True, encoding="utf-8", timeout=60,
            check=True,
        )
        for expected in [
            "Discodex diagnostics", "Presence enabled: false",
            "Discord: Not probed.", "Project names: false", "File names: false",
        ]:
            assert expected in redirected.stdout, expected
        print("Release diagnostics are visible in a parent console and redirected stdout.")


if __name__ == "__main__":
    assert sys.platform == "win32", "This check requires Windows"
    if sys.argv[1] == "--parent-console":
        probe_parent_console(sys.argv[2], sys.argv[3])
    else:
        verify(sys.argv[1])
