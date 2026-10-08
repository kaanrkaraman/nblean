import base64
import io
import json
import sys
from collections.abc import Iterator
from pathlib import Path

import pytest

from nblean import kernel
from nblean.cli import main
from nblean.notebook import NbleanError, Notebook
from nblean.render import Budget, clip, compact_traceback

PNG = base64.b64encode(b"\x89PNG\r\n\x1a\nfake").decode()


def code(source: str, outputs: list[dict[str, object]] | None = None) -> dict[str, object]:
    return {
        "cell_type": "code",
        "id": f"c{abs(hash(source)) % 10**6:06d}",
        "metadata": {},
        "source": source,
        "execution_count": 1 if outputs else None,
        "outputs": outputs or [],
    }


@pytest.fixture
def notebook_path(tmp_path: Path) -> Path:
    cells = [
        {
            "cell_type": "markdown",
            "id": "intro",
            "metadata": {},
            "source": "# Title\nbody",
        },
        code(
            "x = 21",
            [
                {
                    "output_type": "stream",
                    "name": "stdout",
                    "text": "\n".join(map(str, range(100))),
                }
            ],
        ),
        code(
            "plot()",
            [
                {
                    "output_type": "display_data",
                    "data": {"image/png": PNG, "text/plain": "<Figure>"},
                    "metadata": {},
                }
            ],
        ),
        code(
            "boom()",
            [
                {
                    "output_type": "error",
                    "ename": "ValueError",
                    "evalue": "bad",
                    "traceback": ["\x1b[31mframe\x1b[0m", "ValueError: bad"],
                }
            ],
        ),
    ]
    path = tmp_path / "demo.ipynb"
    path.write_text(
        json.dumps({"cells": cells, "metadata": {}, "nbformat": 4, "nbformat_minor": 5})
    )
    return path


@pytest.fixture
def live_kernel(notebook_path: Path) -> Iterator[Path]:
    yield notebook_path
    kernel.stop(notebook_path)


def run(capsys: pytest.CaptureFixture[str], *argv: str) -> tuple[int, str]:
    code = main(argv)
    return code, capsys.readouterr().out


def test_clip_keeps_head_and_tail() -> None:
    text = "\n".join(map(str, range(100)))
    assert clip(text, Budget(lines=4)).split("\n") == [
        "0",
        "1",
        "… 96 lines omitted …",
        "98",
        "99",
    ]
    assert clip(text, Budget(lines=2), tail_only=True).split("\n") == [
        "… 98 lines omitted …",
        "98",
        "99",
    ]


def test_clip_resolves_carriage_returns_and_ansi() -> None:
    assert clip("10%\r50%\r100%\n\x1b[1mdone\x1b[0m", Budget()) == "100%\ndone"


def test_traceback_hides_library_frames() -> None:
    traceback = [
        "\x1b[0;31mKeyError\x1b[0m   Traceback (most recent call last)",
        "Cell In[3], line 1\n----> 1 df['presure']",
        "File /venv/lib/python3.13/site-packages/pandas/core/frame.py:4378, in getitem\n  ...",
        "File /usr/lib/python3.13/json/decoder.py:12, in decode\n  ...",
        "File pandas/_libs/index.pyx:197, in get_loc\n  ...",
        "KeyError: 'presure'",
    ]
    chained = [
        "KeyError   Traceback (most recent call last)",
        "File x.pyx:1",
        "KeyError: 'presure'",
    ]
    assert compact_traceback(chained + traceback) == (
        "KeyError   Traceback (most recent call last)\n"
        "Cell In[3], line 1\n----> 1 df['presure']\n"
        "… 6 library or chained frames omitted …\n"
        "KeyError: 'presure'"
    )


def test_select_specs(notebook_path: Path) -> None:
    notebook = Notebook.load(notebook_path)
    assert notebook.select(["1-2", "all"]) == [1, 2, 0, 3]
    assert notebook.select(["2-"]) == [2, 3]
    assert notebook.select(["intro"]) == [0]
    with pytest.raises(NbleanError):
        notebook.select(["9"])


def test_outline_summarizes_outputs(
    notebook_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    _, out = run(capsys, "outline", str(notebook_path))
    lines = out.splitlines()
    assert lines[0] == "demo.ipynb: 4 cells (1 markdown, 3 code), kernel off"
    assert lines[1] == "  0 md    # Title +1L"
    assert lines[2].endswith("→ 100L")
    assert lines[3].endswith("→ img")
    assert lines[4].endswith("→ ERR ValueError")


def test_show_clips_text_and_extracts_images(
    notebook_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    _, out = run(capsys, "show", str(notebook_path), "1", "2", "--lines", "6")
    assert "… 94 lines omitted …" in out
    image = Path(out.split("[image/png → ")[1].split("]")[0])
    assert image.read_bytes().startswith(b"\x89PNG")


def test_errors_strip_ansi(notebook_path: Path, capsys: pytest.CaptureFixture[str]) -> None:
    _, out = run(capsys, "errors", str(notebook_path))
    assert "frame\nValueError: bad" in out
    assert "\x1b" not in out


def test_edit_round_trip(
    notebook_path: Path,
    capsys: pytest.CaptureFixture[str],
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(sys, "stdin", io.StringIO("y = 2\nprint(y)\n"))
    assert run(capsys, "set", str(notebook_path), "1") == (0, "set 1\n")
    monkeypatch.setattr(sys, "stdin", io.StringIO("## Notes\n"))
    assert run(capsys, "add", str(notebook_path), "--at", "1", "--md") == (
        0,
        "added 1\n",
    )
    assert run(capsys, "rm", str(notebook_path), "3-") == (0, "removed 3, 4\n")
    cells = Notebook.load(notebook_path).cells
    assert [cell["cell_type"] for cell in cells] == ["markdown", "markdown", "code"]
    assert cells[2] | {"id": None} == {
        "cell_type": "code",
        "id": None,
        "metadata": {},
        "source": ["y = 2\n", "print(y)"],
        "execution_count": None,
        "outputs": [],
    }
    assert len(cells[1]["id"]) == 8


def test_kernel_persists_state_and_saves_outputs(
    live_kernel: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    python = sys.executable
    code, out = run(capsys, "run", str(live_kernel), "1", "--python", python)
    assert code == 0
    assert out.startswith("── 1 code id=")
    assert run(capsys, "eval", str(live_kernel), "x * 2")[1] == "42\n"
    code, out = run(capsys, "run", str(live_kernel), "3")
    assert code == 1
    assert "NameError" in out
    cells = Notebook.load(live_kernel).cells
    assert cells[1]["outputs"] == []
    assert cells[1]["execution_count"] == 1
    assert cells[3]["outputs"][0]["ename"] == "NameError"
    assert run(capsys, "kernel", str(live_kernel), "stop") == (0, "stopped\n")
    assert kernel.running_pid(live_kernel) is None


def test_bundled_skill_matches_repo_copy() -> None:
    repo = Path(__file__).parents[1]
    bundled = repo / "src" / "nblean" / "SKILL.md"
    assert bundled.read_text() == (repo / "skills" / "nblean" / "SKILL.md").read_text()


def test_init_writes_each_skill_dir_once(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    monkeypatch.chdir(tmp_path)
    code, out = run(capsys, "init", "--project", "--agent", "codex", "--agent", "opencode")
    assert code == 0
    assert out == f"wrote {tmp_path / '.agents' / 'skills' / 'nblean' / 'SKILL.md'}\n"
    assert run(capsys, "init", "--project", "--agent", "all")[1].count("wrote") == 3
