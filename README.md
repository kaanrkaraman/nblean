<p align="center">
  <strong>nblean</strong><br>
  <strong>Lets coding agents work with Jupyter notebooks without reading every output</strong>
</p>

<p align="center">
  <a href="https://github.com/kaanrkaraman/nblean/actions/workflows/ci.yml"><img src="https://github.com/kaanrkaraman/nblean/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="https://pypi.org/project/nblean/"><img src="https://img.shields.io/pypi/v/nblean" alt="PyPI"></a>
  <a href="https://crates.io/crates/nblean"><img src="https://img.shields.io/crates/v/nblean" alt="crates.io"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/License-MIT-blue.svg" alt="License: MIT"></a>
</p>

<p align="center">
  <a href="#installation">Install</a> &bull;
  <a href="#quick-start">Quick Start</a> &bull;
  <a href="#commands">Commands</a> &bull;
  <a href="#how-savings-work">Benchmarks</a> &bull;
  <a href="#supported-agents">Agents</a>
</p>

---

An `.ipynb` file holds code plus everything the code printed: DataFrame dumps,
progress bars, tracebacks, and plots stored as base64 images. An agent that opens one
reads all of it. To fix one cell, it then runs the whole notebook again and reads the
result again.

nblean is a small CLI and agent skill that lets the agent work one cell at a time. It
lists the notebook as an outline, opens only the cells asked for, clips their outputs,
and runs cells in a background kernel that stays alive between commands.

## What nblean Does

| Agent needs to | Without nblean | With nblean |
|---|---|---|
| See what a notebook contains | Reads the whole JSON, outputs included | `outline`: one line per cell, with an output summary like `→ 14L img ERR KeyError` |
| Look at a few cells | Writes a one-off `json`/`jq` script | `show 12 15-18`: source, plus outputs clipped to the first and last lines |
| Find code | Greps JSON-escaped strings | `grep`: matches by cell and line number |
| Debug | Scrolls through ANSI-colored tracebacks full of library frames | `errors`: only the failing cells, showing your frame and the exception |
| Edit a cell | Rewrites the JSON, or reads the notebook first | `set` / `add` / `rm`, with the new source passed on stdin |
| Re-run after a fix | Runs the whole notebook again and reads all the output | `run 12`: runs that cell in a kernel that is still alive, prints only its output, and saves it into the notebook |
| Check a value | Adds a throwaway `print` cell | `eval 'df.dtypes'`: runs the code in the kernel without changing the notebook |
| Look at plots | Every plot costs image tokens | Plots are saved as files and only their paths are printed; the agent opens one when it needs to |

## How Savings Work

nblean cuts **notebook tokens**, which are only part of an agent's bill. Every session
also pays for the system prompt, tool definitions, and conversation history. These
fixed costs come to about 28k tokens per session on Claude Code. Savings grow with the
size of the notebook and the volume of its outputs.

### A/B test in Claude Code

Claude Sonnet ran headless (`claude -p`), two times with the nblean skill and two
times without it. Token counts
come from the `usage` that Claude Code reports; costs are what the same usage would
cost at API prices.

| Task | Without nblean | With nblean | Change |
|---|---:|---:|---:|
| Answer two questions from the saved outputs of a 7 MB EDA notebook (97 cells, 31 plots) | 236k tokens · $0.207 | 172k tokens · $0.160 | **-27% tokens, -23% cost** |
| Find and fix a bug in a 124 KB notebook, then re-run it until clean | 183k tokens · $0.153 | 183k tokens · $0.150 | no measurable change |

The first version of nblean showed execution counts in the outline, and one run
reported them as cell indices. The numbers above come from runs after that fix, and
all of them answered correctly. Every fix run ended with a clean notebook. On the
small notebook the baseline agent already worked efficiently: it ran `nbconvert` and
grepped for errors, so nblean had little to remove.

### First look

This compares the `outline` output with reading the whole notebook,
counting text as bytes/4 and images by pixel area. It is an upper bound on what the
first step saves.

| Notebook | Whole notebook | `nblean outline` |
|---|---:|---:|
| Flight-data EDA, 97 cells, 31 plots | ~81k | ~1.5k |
| Data preprocessing | ~58k | ~260 |
| Kaggle training run | ~29k | ~90 |

## Installation

```bash
uv tool install nblean                    # PyPI, or: pipx install nblean
brew install kaanrkaraman/tap/nblean      # Homebrew
cargo install nblean                      # crates.io
```

You can also run it without installing: `uvx nblean outline nb.ipynb`.

nblean is a single Rust binary. The PyPI package ships prebuilt wheels for macOS
(Apple Silicon and Intel), Linux (x86_64 and aarch64), and Windows (x64), so
installing it does not pull in any Python dependencies. On other platforms pip builds
it from source, which needs a Rust toolchain.

The kernel runs in the notebook's own environment, not in nblean's. nblean picks the
nearest `.venv` above the notebook, which needs `ipykernel` installed
(`uv add --dev ipykernel`). Use `--python PATH` to pick a different interpreter.

## Quick Start

```bash
# 1. Install the skill for your agent
nblean init                         # Claude Code (default)
nblean init --agent codex           # Codex
nblean init --agent opencode        # OpenCode
nblean init --agent gemini          # Gemini CLI
nblean init --agent all             # all of the above
nblean init --project               # into ./ instead of ~/

# 2. Ask your agent anything about a notebook
```

The skill tells the agent to start with `outline` and never to read the `.ipynb`
directly.

## How It Works

```
  Without nblean:                              With nblean:

  agent --read--> nb.ipynb                     agent --outline/show--> nblean --> nb.ipynb
    ^                 |                          ^                       |  clip, summarize,
    |  every source,  |                          |  only requested       |  images to files
    |  output, image  |                          |  cells, clipped       |
    +-----------------+                          +-----------------------+
                                                 agent --run 12--> nblean --> live kernel
                                                                     |  only this cell's output,
                                                                     |  saved back into nb.ipynb
```

The kernel is a detached `ipykernel` process. Each notebook gets its own kernel, and
each nblean call connects to it, runs code, and disconnects. Variables stay in memory
between calls until `kernel stop` or `kernel restart`.

## Commands

CELL is an index (`12`), a range (`3-7`, `5-`), `all`, or the start of a cell id.

### Read

```bash
nblean outline nb.ipynb                       # one line per cell
nblean show nb.ipynb 12 15-18                 # source + clipped outputs
nblean show nb.ipynb 12 --no-output           # source only
nblean show nb.ipynb 12 --lines 60            # bigger clip; --full disables it
nblean grep nb.ipynb 'read_csv'               # search sources; -o includes outputs
nblean errors nb.ipynb                        # failing cells only
```

### Edit

```bash
nblean set nb.ipynb 12 <<'PY'                 # replace source, clears stale outputs
df = pd.read_parquet(path)
PY
nblean add nb.ipynb --at 13 <<'PY'            # insert; --md for markdown
df.describe()
PY
nblean rm nb.ipynb 14 20-22
```

### Execute

```bash
nblean run nb.ipynb 0-12                      # run cells, save outputs, stop at first error
nblean eval nb.ipynb 'df.shape'               # scratch code, nothing saved
nblean kernel nb.ipynb status|stop|restart
```

## Examples

```
$ nblean outline eda.ipynb
eda.ipynb: 97 cells (36 markdown, 61 code), kernel off
  0 md    1. Files and Schema +14L
  1 code  import itertools +27L
  3 code  files = pl.DataFrame( +5L  → 13L
 13 code  p0 = pl.read_parquet(PARQ / "part.0.parquet") +7L  → 1L img
 15 code  fl_min = feat["flight_length"].to_numpy() / 60 +19L  → img
```

A 40-line pandas traceback, cut down to the line that failed:

```
$ nblean errors buggy.ipynb
── 8 code id=cell08 exec=3 ──
KeyError                                  Traceback (most recent call last)
Cell In[3], line 1
----> 1 df["pressure_kpa"] = df["presure"] * 6.89476
      2 df.head()

… 11 library or chained frames omitted …
KeyError: 'presure'
```

A 300-line training log, with a plot that is saved to a file instead of loaded:

```
$ nblean run buggy.ipynb 3 4 --lines 6
── 3 code id=cell03 exec=4 ──
epoch 000 smoothing pass loss=1.000000 temp_mean=141.845
epoch 001 smoothing pass loss=0.500000 temp_mean=141.724
epoch 002 smoothing pass loss=0.333333 temp_mean=141.651
… 294 lines omitted …
epoch 297 smoothing pass loss=0.003356 temp_mean=139.027
epoch 298 smoothing pass loss=0.003344 temp_mean=139.038
epoch 299 smoothing pass loss=0.003333 temp_mean=139.048
── 4 code id=cell04 exec=5 ──
[image/png → /tmp/nblean/buggy-ec4a859a6390/img/4_0.png]
```

## Supported Agents

The skill uses the [Agent Skills](https://agentskills.io) `SKILL.md` format, so the
same file works in every agent below. The CLI works with any agent that can run shell
commands.

| Agent | `nblean init` | Skill location |
|---|---|---|
| Claude Code | `--agent claude`, or the plugin below | `~/.claude/skills/nblean` |
| Codex | `--agent codex` | `~/.agents/skills/nblean` |
| OpenCode | `--agent opencode` | `~/.agents/skills/nblean` |
| Gemini CLI | `--agent gemini` | `~/.gemini/skills/nblean` |
| Others | `npx skills add kaanrkaraman/nblean` | per agent |

In Claude Code the skill is also available as a plugin:

```
/plugin marketplace add kaanrkaraman/nblean
/plugin install nblean@nblean
```

nblean ships as a CLI and a skill instead of an MCP server. An MCP server's tool
definitions are loaded into every session. A skill is loaded only when a notebook
comes up.

## Speed

Version 0.1 was written in Python. Version 0.2 is the same CLI rewritten in Rust,
and it produces byte-identical output. Startup time dropped from about 90 ms to
2 ms per call. Commands that talk to a running kernel dropped from about 345 ms to
7 ms.

| Command | Python 0.1 | Rust 0.2 |
|---|---:|---:|
| `outline` on a 7 MB notebook | 109 ms | 6 ms |
| `show 0-5` on a 7 MB notebook | 111 ms | 6 ms |
| `eval` against a running kernel | 346 ms | 7 ms |
| `run` one cell against a running kernel | 349 ms | 11 ms |
| starting a kernel | 308 ms | 211 ms |

Measured with hyperfine on an Apple Silicon Mac, mean of at least 20 runs. Kernel
start time is mostly ipykernel's own startup.

## Platform Support

macOS, Linux, and Windows. The kernel's Python can be any version that `ipykernel`
supports.

## Limitations

- A cell's outputs are written when the cell finishes. If a long cell is killed, its output is lost.
- Writes are atomic but not merged. If the notebook is open in Jupyter or VS Code, reload it there after nblean writes.
- Widgets, `update_display_data`, and `input()` are not supported.

## Contributing

```bash
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

The kernel test needs a Python with `ipykernel`. Point `NBLEAN_TEST_PYTHON` at it if
`python3` on your PATH does not have it.

## License

[MIT](LICENSE)
