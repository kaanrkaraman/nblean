from collections.abc import Iterable
from importlib.resources import files
from pathlib import Path

SKILL_DIRS = {
    "claude": Path(".claude", "skills"),
    "codex": Path(".agents", "skills"),
    "opencode": Path(".agents", "skills"),
    "gemini": Path(".gemini", "skills"),
}


def install(agents: Iterable[str], *, project: bool) -> list[Path]:
    names = SKILL_DIRS if "all" in agents else agents
    base = Path.cwd() if project else Path.home()
    targets = dict.fromkeys(base / SKILL_DIRS[name] / "nblean" / "SKILL.md" for name in names)
    text = files("nblean").joinpath("SKILL.md").read_text(encoding="utf-8")
    for target in targets:
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(text, encoding="utf-8", newline="\n")
    return list(targets)
