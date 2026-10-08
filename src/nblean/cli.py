import argparse
import io
import re
import sys
from collections import Counter
from collections.abc import Callable, Sequence
from pathlib import Path
from typing import cast

from nblean import kernel, skill
from nblean.notebook import Cell, NbleanError, Notebook, joined, source_of
from nblean.render import (
    FULL,
    Budget,
    ImageSink,
    Output,
    cell_header,
    clean,
    image_suffix,
    outline_row,
    render_outputs,
    shorten,
)

type Handler = Callable[[argparse.Namespace], int]

GREP_WIDTH = 160


def image_sink(notebook: Path, label: str) -> ImageSink:
    def write(position: int, mime: str, payload: bytes) -> Path:
        directory = kernel.state_dir(notebook) / "img"
        directory.mkdir(parents=True, exist_ok=True)
        target = directory / f"{label}_{position}{image_suffix(mime)}"
        target.write_bytes(payload)
        return target

    return write


def budget_of(args: argparse.Namespace) -> Budget:
    return FULL if args.full else Budget(lines=args.lines)


def read_stdin() -> str:
    if sys.stdin.isatty():
        msg = "expected cell source on stdin"
        raise NbleanError(msg)
    return sys.stdin.read()


def outputs_of(cell: Cell) -> list[Output]:
    return cast("list[Output]", cell.get("outputs", []))


def cmd_outline(args: argparse.Namespace) -> int:
    notebook = Notebook.load(args.notebook)
    kinds = Counter(cell["cell_type"] for cell in notebook.cells)
    summary = ", ".join(f"{count} {kind}" for kind, count in kinds.items())
    status = "live" if kernel.running_pid(notebook.path) else "off"
    print(f"{notebook.path.name}: {len(notebook.cells)} cells ({summary}), kernel {status}")
    for index, cell in enumerate(notebook.cells):
        print(outline_row(index, cell))
    return 0


def cmd_show(args: argparse.Namespace) -> int:
    notebook = Notebook.load(args.notebook)
    budget = budget_of(args)
    for index in notebook.select(args.cells):
        cell = notebook.cells[index]
        print(cell_header(index, cell))
        print(source_of(cell))
        if cell["cell_type"] == "code" and not args.no_output:
            print("── out ──")
            print(render_outputs(outputs_of(cell), budget, image_sink(notebook.path, str(index))))
    return 0


def cmd_grep(args: argparse.Namespace) -> int:
    notebook = Notebook.load(args.notebook)
    pattern = re.compile(args.pattern)
    found = False
    for index, cell in enumerate(notebook.cells):
        texts = [("", source_of(cell))]
        if args.outputs:
            texts += [("out ", text) for text in output_texts(outputs_of(cell))]
        for label, text in texts:
            for number, line in enumerate(clean(text).splitlines(), 1):
                if pattern.search(line):
                    found = True
                    print(f"{index}:{label}{number}: {shorten(line.strip(), GREP_WIDTH)}")
    return 0 if found else 1


def output_texts(outputs: list[Output]) -> list[str]:
    texts = []
    for output in outputs:
        match output:
            case {"output_type": "stream", "text": text} | {"data": {"text/plain": text}}:
                texts.append(joined(text))
            case {"output_type": "error", "traceback": traceback}:
                texts.append("\n".join(traceback))
    return texts


def cmd_errors(args: argparse.Namespace) -> int:
    notebook = Notebook.load(args.notebook)
    budget = budget_of(args)
    failing = 0
    for index, cell in enumerate(notebook.cells):
        errors = [output for output in outputs_of(cell) if output["output_type"] == "error"]
        if errors:
            failing += 1
            print(cell_header(index, cell))
            print(render_outputs(errors, budget, image_sink(notebook.path, str(index))))
    if not failing:
        print("no errors")
    return 0


def cmd_set(args: argparse.Namespace) -> int:
    notebook = Notebook.load(args.notebook)
    match notebook.select([args.cell]):
        case [index]:
            notebook.replace_source(index, read_stdin())
        case indices:
            msg = f"set takes exactly one cell, {args.cell!r} selects {len(indices)}"
            raise NbleanError(msg)
    notebook.save()
    print(f"set {index}")
    return 0


def cmd_add(args: argparse.Namespace) -> int:
    notebook = Notebook.load(args.notebook)
    index = notebook.insert(args.at, "markdown" if args.md else "code", read_stdin())
    notebook.save()
    print(f"added {index}")
    return 0


def cmd_rm(args: argparse.Namespace) -> int:
    notebook = Notebook.load(args.notebook)
    indices = notebook.select(args.cells)
    notebook.remove(indices)
    notebook.save()
    print(f"removed {', '.join(map(str, indices))}")
    return 0


def cmd_run(args: argparse.Namespace) -> int:
    notebook = Notebook.load(args.notebook)
    budget = budget_of(args)
    code_cells = [
        i for i in notebook.select(args.cells) if notebook.cells[i]["cell_type"] == "code"
    ]
    client = kernel.connect(notebook.path, args.python)
    try:
        for index in code_cells:
            cell = notebook.cells[index]
            execution = kernel.execute(client, source_of(cell))
            cell |= {"outputs": execution.outputs, "execution_count": execution.count}
            notebook.save()
            print(cell_header(index, cell))
            print(render_outputs(execution.outputs, budget, image_sink(notebook.path, str(index))))
            if execution.failed:
                return 1
    finally:
        client.stop_channels()
    return 0


def cmd_eval(args: argparse.Namespace) -> int:
    code = read_stdin() if args.code == "-" else args.code
    client = kernel.connect(args.notebook, args.python)
    try:
        execution = kernel.execute(client, code, store_history=False)
    finally:
        client.stop_channels()
    print(render_outputs(execution.outputs, budget_of(args), image_sink(args.notebook, "eval")))
    return 1 if execution.failed else 0


def cmd_kernel(args: argparse.Namespace) -> int:
    match args.action:
        case "status":
            pid = kernel.running_pid(args.notebook)
            print(f"live, pid {pid}" if pid else "off")
        case "stop":
            print("stopped" if kernel.stop(args.notebook) else "was not running")
        case "restart":
            kernel.stop(args.notebook)
            kernel.start(args.notebook, args.python)
            print("restarted, state is empty")
    return 0


def cmd_init(args: argparse.Namespace) -> int:
    for path in skill.install(args.agent or ["claude"], project=args.project):
        print(f"wrote {path}")
    return 0


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="nblean",
        description="Token-lean Jupyter notebook access for coding agents.",
        epilog="CELL is an index, a range like 3-7 or 5-, 'all', or a cell id prefix.",
    )
    commands = parser.add_subparsers(required=True, metavar="COMMAND")

    def command(name: str, handler: Handler, summary: str) -> argparse.ArgumentParser:
        sub = commands.add_parser(name, help=summary, description=summary)
        sub.add_argument("notebook", type=Path)
        sub.set_defaults(handler=handler)
        return sub

    def output_options(sub: argparse.ArgumentParser) -> None:
        sub.add_argument("--full", action="store_true", help="disable output clipping")
        sub.add_argument("--lines", type=int, default=20, help="max lines per output (default 20)")

    def kernel_options(sub: argparse.ArgumentParser) -> None:
        sub.add_argument("--python", type=Path, help="kernel interpreter (default: nearest .venv)")

    init = commands.add_parser(
        "init", help="install the agent skill", description="Install the nblean agent skill."
    )
    init.add_argument(
        "--agent",
        action="append",
        choices=[*skill.SKILL_DIRS, "all"],
        help="target agent, repeatable (default: claude)",
    )
    init.add_argument("--project", action="store_true", help="install into ./ instead of ~/")
    init.set_defaults(handler=cmd_init)

    command("outline", cmd_outline, "one line per cell with output summary")

    show = command("show", cmd_show, "source and clipped outputs of selected cells")
    show.add_argument("cells", nargs="+", metavar="CELL")
    show.add_argument("--no-output", action="store_true", help="source only")
    output_options(show)

    grep = command("grep", cmd_grep, "regex search over cell sources")
    grep.add_argument("pattern")
    grep.add_argument("-o", "--outputs", action="store_true", help="search outputs too")

    output_options(command("errors", cmd_errors, "tracebacks of failing cells"))

    command("set", cmd_set, "replace a cell's source from stdin").add_argument(
        "cell", metavar="CELL"
    )

    add = command("add", cmd_add, "insert a cell from stdin")
    add.add_argument("--at", type=int, help="insert position (default: end)")
    add.add_argument("--md", action="store_true", help="markdown cell")

    command("rm", cmd_rm, "delete cells").add_argument("cells", nargs="+", metavar="CELL")

    run = command("run", cmd_run, "execute cells in a persistent kernel and save outputs")
    run.add_argument("cells", nargs="+", metavar="CELL")
    output_options(run)
    kernel_options(run)

    evaluate = command("eval", cmd_eval, "run scratch code in the kernel without saving it")
    evaluate.add_argument("code", help="code to run, or - for stdin")
    output_options(evaluate)
    kernel_options(evaluate)

    manage = command("kernel", cmd_kernel, "inspect or control the notebook's kernel")
    manage.add_argument("action", choices=["status", "stop", "restart"])
    kernel_options(manage)

    return parser


def main(argv: Sequence[str] | None = None) -> int:
    if sys.platform == "win32":
        for stream in (sys.stdout, sys.stderr):
            if isinstance(stream, io.TextIOWrapper):
                stream.reconfigure(encoding="utf-8", errors="replace")
    args = build_parser().parse_args(argv)
    try:
        handler: Handler = args.handler
        return handler(args)
    except NbleanError as exc:
        print(f"nblean: {exc}", file=sys.stderr)
        return 2
