import base64
import mimetypes
import re
from collections import Counter
from collections.abc import Callable
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from nblean.notebook import Cell, joined, source_of

type Output = dict[str, Any]
type ImageSink = Callable[[int, str, bytes], Path]

ANSI_ESCAPE = re.compile(r"\x1b\[[0-?]*[ -/]*[@-~]")
HEAD_WIDTH = 72
LIBRARY_FRAME = re.compile(r"site-packages|\.pyx\b|\.pxi\b|[\\/]lib[\\/]python\d|[\\/]Lib[\\/]")
TRACEBACK_HEADER = "Traceback (most recent call last)"


@dataclass(frozen=True, slots=True)
class Budget:
    lines: int | None = 20
    width: int | None = 160


FULL = Budget(lines=None, width=None)


def shorten(line: str, width: int | None) -> str:
    return line if width is None or len(line) <= width else f"{line[: width - 1]}…"


def clean(text: str) -> str:
    text = ANSI_ESCAPE.sub("", text)
    return "\n".join(line.rstrip("\r").rsplit("\r", 1)[-1] for line in text.split("\n"))


def clip(text: str, budget: Budget, *, tail_only: bool = False) -> str:
    lines = [shorten(line, budget.width) for line in clean(text).rstrip("\n").split("\n")]
    if budget.lines is None or len(lines) <= budget.lines:
        return "\n".join(lines)
    head = 0 if tail_only else budget.lines // 2
    tail = budget.lines - head
    marker = f"… {len(lines) - budget.lines} lines omitted …"
    return "\n".join([*lines[:head], marker, *lines[-tail:]])


def compact_traceback(traceback: list[str]) -> str:
    entries = [clean(entry) for entry in traceback]
    start = max((i for i, entry in enumerate(entries) if TRACEBACK_HEADER in entry), default=0)
    *frames, exception = entries[start:]
    kept = [frame for frame in frames if not LIBRARY_FRAME.search(frame.split("\n", 1)[0])]
    hidden = len(entries) - 1 - len(kept)
    marker = [f"… {hidden} library or chained frames omitted …"] if hidden else []
    return "\n".join([*kept, *marker, exception])


def render_error(output: Output, budget: Budget) -> str:
    traceback: list[str] = output.get("traceback", [])
    if not traceback:
        return f"{output.get('ename')}: {output.get('evalue')}"
    text = "\n".join(traceback) if budget.lines is None else compact_traceback(traceback)
    return clip(text, budget, tail_only=True)


def line_count(text: str) -> int:
    return clean(text).rstrip("\n").count("\n") + 1


def image_mime(data: dict[str, Any]) -> str | None:
    return next((mime for mime in data if mime.startswith("image/")), None)


def image_bytes(mime: str, payload: str | list[str]) -> bytes:
    text = joined(payload)
    return text.encode() if mime == "image/svg+xml" else base64.b64decode(text)


def image_suffix(mime: str) -> str:
    return mimetypes.guess_extension(mime) or ".bin"


def render_output(position: int, output: Output, budget: Budget, sink: ImageSink) -> str:
    match output:
        case {"output_type": "stream", "text": text}:
            return clip(joined(text), budget)
        case {"output_type": "error"}:
            return render_error(output, budget)
        case {"data": data} if mime := image_mime(data):
            return f"[{mime} → {sink(position, mime, image_bytes(mime, data[mime]))}]"
        case {"data": {"text/plain": text}}:
            return clip(joined(text), budget)
        case {"data": data}:
            return f"[{', '.join(data)}]"
        case _:
            return f"[{output.get('output_type', 'unknown output')}]"


def render_outputs(outputs: list[Output], budget: Budget, sink: ImageSink) -> str:
    rendered = (render_output(n, output, budget, sink) for n, output in enumerate(outputs))
    return "\n".join(rendered) or "(no output)"


def summarize(outputs: list[Output]) -> str:
    text_lines = 0
    marks: Counter[str] = Counter()
    for output in outputs:
        match output:
            case {"output_type": "error", "ename": ename}:
                marks[f"ERR {ename}"] += 1
            case {"output_type": "stream", "text": text}:
                text_lines += line_count(joined(text))
            case {"data": data} if image_mime(data):
                marks["img"] += 1
            case {"data": {"text/plain": text}}:
                text_lines += line_count(joined(text))
            case _:
                marks["other"] += 1
    parts = [f"{text_lines}L"] if text_lines else []
    parts += [mark if n == 1 else f"{mark}×{n}" for mark, n in marks.items()]
    return " ".join(parts)


def outline_row(index: int, cell: Cell) -> str:
    lines = [line.strip() for line in source_of(cell).splitlines() if line.strip()]
    head = shorten(lines[0], HEAD_WIDTH) if lines else ""
    extra = f" +{len(lines) - 1}L" if len(lines) > 1 else ""
    suffix = ""
    match cell["cell_type"]:
        case "code":
            tag = "code"
            if cell.get("execution_count") is None:
                suffix = "  → not run"
            elif summary := summarize(cell.get("outputs", [])):
                suffix = f"  → {summary}"
        case "markdown":
            tag = "md"
        case kind:
            tag = kind
    return f"{index:>3} {tag:<5} {head}{extra}{suffix}"


def cell_header(index: int, cell: Cell) -> str:
    fields = [str(index), cell["cell_type"]]
    if cell_id := cell.get("id"):
        fields.append(f"id={cell_id}")
    if (count := cell.get("execution_count")) is not None:
        fields.append(f"exec={count}")
    return f"── {' '.join(fields)} ──"
