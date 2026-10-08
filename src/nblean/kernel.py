import hashlib
import json
import os
import subprocess
import sys
import tempfile
import time
from contextlib import suppress
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from jupyter_client.blocking.client import BlockingKernelClient

from nblean import process
from nblean.notebook import NbleanError
from nblean.render import Output

STATE_ROOT = Path(tempfile.gettempdir()) / "nblean"
STARTUP_TIMEOUT = 60.0
POLL_INTERVAL = 0.1
LOG_TAIL = 15
RESULT_KEYS = ("data", "execution_count", "metadata")
ERROR_KEYS = ("ename", "evalue", "traceback")


@dataclass(slots=True)
class Execution:
    outputs: list[Output] = field(default_factory=list)
    count: int | None = None

    @property
    def failed(self) -> bool:
        return any(output["output_type"] == "error" for output in self.outputs)

    def collect(self, message: dict[str, Any]) -> None:
        content = message["content"]
        match message["msg_type"]:
            case "stream":
                last = self.outputs[-1] if self.outputs else {}
                if last.get("output_type") == "stream" and last["name"] == content["name"]:
                    last["text"] += content["text"]
                else:
                    self.outputs.append(
                        {
                            "output_type": "stream",
                            "name": content["name"],
                            "text": content["text"],
                        }
                    )
            case "display_data":
                self.outputs.append(
                    {
                        "output_type": "display_data",
                        "data": content["data"],
                        "metadata": content["metadata"],
                    }
                )
            case "execute_result":
                self.outputs.append(
                    {"output_type": "execute_result"} | {key: content[key] for key in RESULT_KEYS}
                )
            case "error":
                self.outputs.append(
                    {"output_type": "error"} | {key: content[key] for key in ERROR_KEYS}
                )
            case "clear_output":
                self.outputs.clear()


def state_dir(notebook: Path) -> Path:
    digest = hashlib.sha256(str(notebook.resolve()).encode()).hexdigest()[:12]
    return STATE_ROOT / f"{notebook.stem}-{digest}"


def find_python(start: Path) -> Path:
    for directory in (start.resolve(), *start.resolve().parents):
        if (candidate := directory / ".venv" / process.VENV_PYTHON).exists():
            return candidate
    return Path(sys.executable)


def running_pid(notebook: Path) -> int | None:
    state = state_dir(notebook)
    try:
        pid = int((state / "kernel.pid").read_text())
    except (OSError, ValueError):
        return None
    return pid if process.is_running(pid) and (state / "kernel.json").exists() else None


def start(notebook: Path, python: Path | None) -> None:
    state = state_dir(notebook)
    state.mkdir(parents=True, exist_ok=True)
    connection = state / "kernel.json"
    connection.unlink(missing_ok=True)
    interpreter = python or find_python(notebook.parent)
    environment = {key: value for key, value in os.environ.items() if key != "JPY_PARENT_PID"}
    log_path = state / "kernel.log"
    with log_path.open("w") as log:
        kernel = subprocess.Popen(
            [str(interpreter), "-m", "ipykernel_launcher", "-f", str(connection)],
            cwd=notebook.resolve().parent,
            env=environment,
            stdin=subprocess.DEVNULL,
            stdout=log,
            stderr=subprocess.STDOUT,
            start_new_session=process.NEW_SESSION,
            creationflags=process.CREATION_FLAGS,
        )
    (state / "kernel.pid").write_text(str(kernel.pid))
    deadline = time.monotonic() + STARTUP_TIMEOUT
    while time.monotonic() < deadline:
        if kernel.poll() is not None:
            tail = "\n".join(log_path.read_text().splitlines()[-LOG_TAIL:])
            msg = (
                f"kernel exited using {interpreter}:\n{tail}\n"
                "is ipykernel installed there? `uv add --dev ipykernel`"
            )
            raise NbleanError(msg)
        with suppress(OSError, ValueError):
            json.loads(connection.read_text())
            return
        time.sleep(POLL_INTERVAL)
    kernel.kill()
    msg = f"kernel did not start within {STARTUP_TIMEOUT:.0f}s, see {log_path}"
    raise NbleanError(msg)


def stop(notebook: Path) -> bool:
    pid = running_pid(notebook)
    if pid is not None:
        process.terminate_tree(pid)
    for name in ("kernel.json", "kernel.pid"):
        (state_dir(notebook) / name).unlink(missing_ok=True)
    return pid is not None


def connect(notebook: Path, python: Path | None) -> BlockingKernelClient:
    if running_pid(notebook) is None:
        start(notebook, python)
    client = BlockingKernelClient()
    client.load_connection_file(str(state_dir(notebook) / "kernel.json"))
    client.start_channels()
    client.wait_for_ready(timeout=STARTUP_TIMEOUT)
    return client


def execute(client: BlockingKernelClient, code: str, *, store_history: bool = True) -> Execution:
    execution = Execution()
    reply = client.execute_interactive(
        code,
        store_history=store_history,
        allow_stdin=False,
        output_hook=execution.collect,
    )
    execution.count = reply["content"].get("execution_count")
    return execution
