use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use serde_json::{Value, json};
use tempfile::TempDir;

const PNG: &str = "iVBORw0KGgpmYWtl";

fn nblean(args: &[&str], stdin: Option<&str>, cwd: Option<&Path>) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_nblean"));
    command
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    let mut child = command.spawn().expect("binary under test should launch");
    let mut input = child.stdin.take().expect("stdin was piped");
    input
        .write_all(stdin.unwrap_or_default().as_bytes())
        .expect("stdin should accept input");
    drop(input);
    child
        .wait_with_output()
        .expect("binary under test should finish")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn code_cell(id: &str, source: &str, outputs: &Value) -> Value {
    let ran = outputs
        .as_array()
        .is_some_and(|outputs| !outputs.is_empty());
    json!({
        "cell_type": "code", "id": id, "metadata": {}, "source": source,
        "execution_count": if ran { json!(1) } else { Value::Null }, "outputs": outputs
    })
}

fn demo_notebook() -> (TempDir, PathBuf) {
    let log = (0..100)
        .map(|n| n.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    let cells = json!([
        {"cell_type": "markdown", "id": "intro", "metadata": {}, "source": "# Title\nbody"},
        code_cell("c1", "x = 21", &json!([{"output_type": "stream", "name": "stdout", "text": log}])),
        code_cell("c2", "plot()", &json!([{"output_type": "display_data", "metadata": {},
            "data": {"image/png": PNG, "text/plain": "<Figure>"}}])),
        code_cell("c3", "boom()", &json!([{"output_type": "error", "ename": "ValueError", "evalue": "bad",
            "traceback": ["\u{1b}[31mframe\u{1b}[0m", "ValueError: bad"]}])),
    ]);
    let directory = TempDir::new().expect("temp dir");
    let path = directory.path().join("demo.ipynb");
    let notebook = json!({"cells": cells, "metadata": {}, "nbformat": 4, "nbformat_minor": 5});
    std::fs::write(&path, notebook.to_string()).expect("write notebook");
    (directory, path)
}

fn load(path: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path).expect("read notebook"))
        .expect("valid json")
}

#[test]
fn outline_summarizes_outputs() {
    let (_directory, path) = demo_notebook();
    let output = nblean(&["outline", path.to_str().expect("utf-8 path")], None, None);
    let text = stdout(&output);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(
        lines[0],
        "demo.ipynb: 4 cells (1 markdown, 3 code), kernel off"
    );
    assert_eq!(lines[1], "  0 md    # Title +1L");
    assert!(lines[2].ends_with("→ 100L"));
    assert!(lines[3].ends_with("→ img"));
    assert!(lines[4].ends_with("→ ERR ValueError"));
}

#[test]
fn show_clips_text_and_extracts_images() {
    let (_directory, path) = demo_notebook();
    let output = nblean(
        &[
            "show",
            path.to_str().expect("utf-8 path"),
            "1",
            "2",
            "--lines",
            "6",
        ],
        None,
        None,
    );
    let text = stdout(&output);
    assert!(text.contains("… 94 lines omitted …"));
    let image = text
        .split("[image/png → ")
        .nth(1)
        .and_then(|rest| rest.split(']').next())
        .expect("image path");
    assert!(
        std::fs::read(image)
            .expect("image file")
            .starts_with(b"\x89PNG")
    );
}

#[test]
fn errors_strip_ansi() {
    let (_directory, path) = demo_notebook();
    let text = stdout(&nblean(
        &["errors", path.to_str().expect("utf-8 path")],
        None,
        None,
    ));
    assert!(text.contains("frame\nValueError: bad"));
    assert!(!text.contains('\u{1b}'));
}

#[test]
fn select_rejects_out_of_range_cells() {
    let (_directory, path) = demo_notebook();
    let output = nblean(
        &["show", path.to_str().expect("utf-8 path"), "9"],
        None,
        None,
    );
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("cell 9 out of range"));
}

#[test]
fn grep_reports_cell_and_line() {
    let (_directory, path) = demo_notebook();
    let output = nblean(
        &["grep", path.to_str().expect("utf-8 path"), "boom"],
        None,
        None,
    );
    assert_eq!(stdout(&output), "3:1: boom()\n");
    let missing = nblean(
        &["grep", path.to_str().expect("utf-8 path"), "absent"],
        None,
        None,
    );
    assert_eq!(missing.status.code(), Some(1));
}

#[test]
fn edit_round_trip() {
    let (_directory, path) = demo_notebook();
    let notebook = path.to_str().expect("utf-8 path");
    assert_eq!(
        stdout(&nblean(
            &["set", notebook, "1"],
            Some("y = 2\nprint(y)\n"),
            None
        )),
        "set 1\n"
    );
    assert_eq!(
        stdout(&nblean(
            &["add", notebook, "--at", "1", "--md"],
            Some("## Notes\n"),
            None
        )),
        "added 1\n"
    );
    assert_eq!(
        stdout(&nblean(&["rm", notebook, "3-"], None, None)),
        "removed 3, 4\n"
    );
    let cells = load(&path)["cells"].clone();
    let kinds: Vec<&str> = cells
        .as_array()
        .expect("cells")
        .iter()
        .filter_map(|c| c["cell_type"].as_str())
        .collect();
    assert_eq!(kinds, ["markdown", "markdown", "code"]);
    assert_eq!(cells[2]["source"], json!(["y = 2\n", "print(y)"]));
    assert_eq!(cells[2]["outputs"], json!([]));
    assert!(cells[2]["execution_count"].is_null());
    assert_eq!(cells[1]["id"].as_str().map(str::len), Some(8));
}

#[test]
fn init_writes_each_skill_dir_once() {
    let directory = TempDir::new().expect("temp dir");
    let output = nblean(
        &[
            "init",
            "--project",
            "--agent",
            "codex",
            "--agent",
            "opencode",
        ],
        None,
        Some(directory.path()),
    );
    let expected = directory
        .path()
        .join(".agents")
        .join("skills")
        .join("nblean")
        .join("SKILL.md");
    let written = stdout(&output);
    let relative = Path::new(".agents")
        .join("skills")
        .join("nblean")
        .join("SKILL.md");
    assert_eq!(written.lines().count(), 1);
    assert!(
        written
            .trim_end()
            .ends_with(&relative.display().to_string())
    );
    assert!(
        std::fs::read_to_string(expected)
            .expect("skill file")
            .contains("name: nblean")
    );
    let all = nblean(
        &["init", "--project", "--agent", "all"],
        None,
        Some(directory.path()),
    );
    assert_eq!(stdout(&all).matches("wrote").count(), 3);
}

#[test]
fn kernel_persists_state_and_saves_outputs() {
    let python = std::env::var("NBLEAN_TEST_PYTHON").unwrap_or_else(|_| {
        if cfg!(windows) {
            "python".to_owned()
        } else {
            "python3".to_owned()
        }
    });
    let (_directory, path) = demo_notebook();
    let notebook = path.to_str().expect("utf-8 path");
    let run = nblean(&["run", notebook, "1", "--python", &python], None, None);
    assert_eq!(
        run.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(stdout(&run).starts_with("── 1 code id=c1"));
    assert_eq!(
        stdout(&nblean(&["eval", notebook, "x * 2"], None, None)),
        "42\n"
    );
    let failing = nblean(&["run", notebook, "3"], None, None);
    assert_eq!(failing.status.code(), Some(1));
    assert!(stdout(&failing).contains("NameError"));
    let cells = load(&path)["cells"].clone();
    assert_eq!(cells[1]["outputs"], json!([]));
    assert_eq!(cells[1]["execution_count"], json!(1));
    assert_eq!(cells[3]["outputs"][0]["ename"], "NameError");
    assert_eq!(
        stdout(&nblean(&["kernel", notebook, "stop"], None, None)),
        "stopped\n"
    );
    assert_eq!(
        stdout(&nblean(&["kernel", notebook, "status"], None, None)),
        "off\n"
    );
}
