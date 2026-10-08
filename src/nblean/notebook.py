import json
import re
import uuid
from collections.abc import Iterable
from dataclasses import dataclass
from itertools import chain
from pathlib import Path
from typing import Any, Self, cast

type Cell = dict[str, Any]

CELL_RANGE = re.compile(r"(\d+)-(\d*)")


class NbleanError(Exception):
    pass


def joined(value: str | list[str]) -> str:
    return value if isinstance(value, str) else "".join(value)


def source_of(cell: Cell) -> str:
    return joined(cell["source"])


def lines_of(text: str) -> list[str]:
    return text.removesuffix("\n").splitlines(keepends=True)


@dataclass(slots=True)
class Notebook:
    path: Path
    data: dict[str, Any]

    @classmethod
    def load(cls, path: Path) -> Self:
        try:
            data = json.loads(path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError) as exc:
            msg = f"{path}: {exc}"
            raise NbleanError(msg) from exc
        return cls(path, data)

    @property
    def cells(self) -> list[Cell]:
        return cast("list[Cell]", self.data["cells"])

    @property
    def has_cell_ids(self) -> bool:
        return (self.data.get("nbformat", 4), self.data.get("nbformat_minor", 0)) >= (
            4,
            5,
        )

    def save(self) -> None:
        text = json.dumps(self.data, sort_keys=True, indent=1, ensure_ascii=False) + "\n"
        staging = self.path.with_name(f".{self.path.name}.nblean")
        staging.write_text(text, encoding="utf-8", newline="\n")
        staging.replace(self.path)

    def select(self, specs: Iterable[str]) -> list[int]:
        return list(dict.fromkeys(chain.from_iterable(map(self._resolve, specs))))

    def _resolve(self, spec: str) -> Iterable[int]:
        count = len(self.cells)
        if spec == "all":
            return range(count)
        if spec.isdigit():
            if (index := int(spec)) >= count:
                msg = f"cell {index} out of range, notebook has {count} cells"
                raise NbleanError(msg)
            return [index]
        if match := CELL_RANGE.fullmatch(spec):
            stop = int(match[2]) + 1 if match[2] else count
            return range(int(match[1]), min(stop, count))
        hits = [i for i, cell in enumerate(self.cells) if cell.get("id", "").startswith(spec)]
        if len(hits) != 1:
            msg = f"cell id {spec!r} matched {len(hits)} cells"
            raise NbleanError(msg)
        return hits

    def replace_source(self, index: int, text: str) -> None:
        cell = self.cells[index]
        cell["source"] = lines_of(text)
        if cell["cell_type"] == "code":
            cell |= {"execution_count": None, "outputs": []}

    def insert(self, index: int | None, kind: str, text: str) -> int:
        cell: Cell = {"cell_type": kind, "metadata": {}, "source": lines_of(text)}
        if self.has_cell_ids:
            cell["id"] = uuid.uuid4().hex[:8]
        if kind == "code":
            cell |= {"execution_count": None, "outputs": []}
        position = len(self.cells) if index is None else min(index, len(self.cells))
        self.cells.insert(position, cell)
        return position

    def remove(self, indices: Iterable[int]) -> None:
        for index in sorted(indices, reverse=True):
            del self.cells[index]
