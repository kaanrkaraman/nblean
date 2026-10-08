---
name: nblean
description: Use whenever a task touches a Jupyter notebook (.ipynb), including reading, searching, editing, running cells, debugging tracebacks, and inspecting plots. Run the nblean CLI instead of reading the notebook, so only the cells you need enter context, with their outputs clipped.
---

# Working with notebooks via nblean

Never `Read`, `cat`, `grep`, or `jq` an `.ipynb` file directly: it loads every output,
every base64 image, and every progress-bar line. Use `nblean`; if it is not on PATH, run it as `uvx nblean`.

## Navigate

```bash
nblean outline nb.ipynb             # always first: one line per cell, output summary
nblean show nb.ipynb 12 15-18       # source + clipped outputs of those cells only
nblean show nb.ipynb 12 --no-output # source only, when you are about to edit
nblean grep nb.ipynb 'read_csv'     # index:line hits; -o searches outputs too
nblean errors nb.ipynb              # failing cells only, with your frame and the exception
```

CELL is an index, a range (`3-7`, `5-`), `all`, or a cell id prefix.
Outputs are clipped to 20 lines (`--lines N`, `--full` to disable). Ask for more
only when the clipped part is what you need.

## Edit

```bash
nblean set nb.ipynb 12 <<'PY'
df = load(path)
PY
nblean add nb.ipynb --at 13 <<'PY'      # --md for markdown; omit --at to append
df.describe()
PY
nblean rm nb.ipynb 14
```

Indices shift after `add`/`rm`; re-run `outline` or use ids before chaining edits.
`set` clears the cell's stale outputs.

## Execute

```bash
nblean run nb.ipynb 0-12        # persistent kernel, outputs saved into the notebook
nblean run nb.ipynb 12          # re-run one edited cell; earlier state is kept
nblean eval nb.ipynb 'df.dtypes' # scratch probe, not saved to the notebook
nblean kernel nb.ipynb restart  # clean state; also status, stop
```

- The kernel uses the nearest `.venv/bin/python` (override `--python`) and the
  notebook's directory as cwd. It needs `ipykernel` in that env.
- A fresh kernel is empty: run the prerequisite cells before the one you changed.
- Investigate with `eval` instead of inserting throwaway print cells.
- Cells longer than a couple of minutes: run with Bash `run_in_background`.
- `run` stops at the first error and exits 1.
- Stop the kernel when the task is done.

## Images

Plots appear as `[image/png → /tmp/.../12_0.png]`. Do not open them by default.
Prefer numbers: `nblean eval nb.ipynb 'series.describe()'`. Open an image with `Read`
only when the judgement is visual, and downscale first since image tokens scale
with pixel area: `sips -Z 768 in.png --out /tmp/small.png` (macOS).

## Caveats

If the user has the notebook open in Jupyter or VS Code, tell them to reload it after
your writes and not to save over them from the editor.
