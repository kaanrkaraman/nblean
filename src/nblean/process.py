import os
import subprocess
import sys
from contextlib import suppress
from pathlib import Path

if sys.platform == "win32":
    import ctypes

    PROCESS_QUERY_LIMITED_INFORMATION = 0x1000
    STILL_ACTIVE = 259
    VENV_PYTHON = Path("Scripts", "python.exe")
    CREATION_FLAGS = subprocess.CREATE_NEW_PROCESS_GROUP | subprocess.DETACHED_PROCESS
    NEW_SESSION = False

    def is_running(pid: int) -> bool:
        kernel32 = ctypes.windll.kernel32
        handle = kernel32.OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, False, pid)
        if not handle:
            return False
        try:
            code = ctypes.c_ulong()
            alive = kernel32.GetExitCodeProcess(handle, ctypes.byref(code))
            return bool(alive) and code.value == STILL_ACTIVE
        finally:
            kernel32.CloseHandle(handle)

    def terminate_tree(pid: int) -> None:
        subprocess.run(["taskkill", "/F", "/T", "/PID", str(pid)], capture_output=True, check=False)

else:
    import signal

    VENV_PYTHON = Path("bin", "python")
    CREATION_FLAGS = 0
    NEW_SESSION = True

    def is_running(pid: int) -> bool:
        try:
            os.kill(pid, 0)
        except OSError:
            return False
        return True

    def terminate_tree(pid: int) -> None:
        with suppress(ProcessLookupError):
            os.killpg(pid, signal.SIGTERM)
