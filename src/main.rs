mod kernel;
mod notebook;
mod render;

use std::fmt::Arguments;
use std::fs;
use std::io::{ErrorKind, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::bail;
use clap::{Args, Parser, Subcommand, ValueEnum};
use regex::Regex;
use serde_json::Value;

use crate::kernel::Client;
use crate::notebook::{Notebook, joined, source_of};
use crate::render::{
    Budget, cell_header, clean, image_suffix, outline_row, render_outputs, shorten,
};

const SKILL: &str = include_str!("../skills/nblean/SKILL.md");
const GREP_WIDTH: usize = 160;

#[derive(Parser)]
#[command(
    name = "nblean",
    about = "Token-lean Jupyter notebook access for coding agents.",
    after_help = "CELL is an index, a range like 3-7 or 5-, 'all', or a cell id prefix."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Args)]
struct OutputArgs {
    #[arg(long, help = "disable output clipping")]
    full: bool,
    #[arg(long, default_value_t = 20, help = "max lines per output")]
    lines: usize,
}

impl OutputArgs {
    fn budget(&self) -> Budget {
        if self.full {
            Budget::FULL
        } else {
            Budget::clipped(self.lines)
        }
    }
}

#[derive(Args)]
struct KernelArgs {
    #[arg(long, help = "kernel interpreter (default: nearest .venv)")]
    python: Option<PathBuf>,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Agent {
    Claude,
    Codex,
    Opencode,
    Gemini,
    All,
}

#[derive(Clone, Copy, ValueEnum)]
enum KernelAction {
    Status,
    Stop,
    Restart,
}

#[derive(Subcommand)]
enum Command {
    #[command(about = "install the agent skill")]
    Init {
        #[arg(long, value_enum, help = "target agent, repeatable (default: claude)")]
        agent: Vec<Agent>,
        #[arg(long, help = "install into ./ instead of ~/")]
        project: bool,
    },
    #[command(about = "one line per cell with output summary")]
    Outline { notebook: PathBuf },
    #[command(about = "source and clipped outputs of selected cells")]
    Show {
        notebook: PathBuf,
        #[arg(required = true, value_name = "CELL")]
        cells: Vec<String>,
        #[arg(long, help = "source only")]
        no_output: bool,
        #[command(flatten)]
        output: OutputArgs,
    },
    #[command(about = "regex search over cell sources")]
    Grep {
        notebook: PathBuf,
        pattern: String,
        #[arg(short = 'o', long, help = "search outputs too")]
        outputs: bool,
    },
    #[command(about = "tracebacks of failing cells")]
    Errors {
        notebook: PathBuf,
        #[command(flatten)]
        output: OutputArgs,
    },
    #[command(about = "replace a cell's source from stdin")]
    Set {
        notebook: PathBuf,
        #[arg(value_name = "CELL")]
        cell: String,
    },
    #[command(about = "insert a cell from stdin")]
    Add {
        notebook: PathBuf,
        #[arg(long, help = "insert position (default: end)")]
        at: Option<usize>,
        #[arg(long, help = "markdown cell")]
        md: bool,
    },
    #[command(about = "delete cells")]
    Rm {
        notebook: PathBuf,
        #[arg(required = true, value_name = "CELL")]
        cells: Vec<String>,
    },
    #[command(about = "execute cells in a persistent kernel and save outputs")]
    Run {
        notebook: PathBuf,
        #[arg(required = true, value_name = "CELL")]
        cells: Vec<String>,
        #[command(flatten)]
        output: OutputArgs,
        #[command(flatten)]
        kernel: KernelArgs,
    },
    #[command(about = "run scratch code in the kernel without saving it")]
    Eval {
        notebook: PathBuf,
        #[arg(help = "code to run, or - for stdin")]
        code: String,
        #[command(flatten)]
        output: OutputArgs,
        #[command(flatten)]
        kernel: KernelArgs,
    },
    #[command(about = "inspect or control the notebook's kernel")]
    Kernel {
        notebook: PathBuf,
        #[arg(value_enum)]
        action: KernelAction,
        #[command(flatten)]
        kernel: KernelArgs,
    },
}

fn emit(line: Arguments<'_>) {
    if let Err(error) = writeln!(std::io::stdout().lock(), "{line}") {
        std::process::exit(if error.kind() == ErrorKind::BrokenPipe {
            0
        } else {
            2
        });
    }
}

fn image_sink(
    notebook: &Path,
    label: String,
) -> impl FnMut(usize, &str, &[u8]) -> anyhow::Result<PathBuf> {
    let directory = kernel::state_dir(notebook).join("img");
    move |position, mime, payload| {
        fs::create_dir_all(&directory)?;
        let target = directory.join(format!("{label}_{position}{}", image_suffix(mime)));
        fs::write(&target, payload)?;
        Ok(target)
    }
}

fn read_stdin() -> anyhow::Result<String> {
    let mut stdin = std::io::stdin();
    if stdin.is_terminal() {
        bail!("expected cell source on stdin");
    }
    let mut text = String::new();
    stdin.read_to_string(&mut text)?;
    Ok(text)
}

fn outputs_of(cell: &Value) -> &[Value] {
    cell["outputs"].as_array().map_or(&[], Vec::as_slice)
}

fn output_texts(outputs: &[Value]) -> Vec<String> {
    outputs
        .iter()
        .filter_map(|output| match output["output_type"].as_str() {
            Some("stream") => Some(joined(&output["text"])),
            Some("error") => Some(
                output["traceback"]
                    .as_array()
                    .map(|frames| {
                        frames
                            .iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                    .unwrap_or_default(),
            ),
            _ => output["data"].get("text/plain").map(joined),
        })
        .collect()
}

fn skill_dir(agent: Agent) -> PathBuf {
    let home = match agent {
        Agent::Claude => ".claude",
        Agent::Codex | Agent::Opencode => ".agents",
        Agent::Gemini | Agent::All => ".gemini",
    };
    Path::new(home).join("skills")
}

fn cmd_init(agents: &[Agent], project: bool) -> anyhow::Result<u8> {
    let chosen: Vec<Agent> = if agents.is_empty() {
        vec![Agent::Claude]
    } else if agents.contains(&Agent::All) {
        vec![Agent::Claude, Agent::Codex, Agent::Opencode, Agent::Gemini]
    } else {
        agents.to_vec()
    };
    let base = if project {
        std::env::current_dir()?
    } else {
        std::env::home_dir().unwrap_or_default()
    };
    let mut written: Vec<PathBuf> = Vec::new();
    for agent in chosen {
        let target = base.join(skill_dir(agent)).join("nblean").join("SKILL.md");
        if written.contains(&target) {
            continue;
        }
        fs::create_dir_all(target.parent().unwrap_or(&base))?;
        fs::write(&target, SKILL)?;
        emit(format_args!("wrote {}", target.display()));
        written.push(target);
    }
    Ok(0)
}

fn cmd_outline(path: &Path) -> anyhow::Result<u8> {
    let notebook = Notebook::load(path)?;
    let mut kinds: Vec<(&str, usize)> = Vec::new();
    for cell in notebook.cells() {
        let kind = cell["cell_type"].as_str().unwrap_or_default();
        match kinds.iter_mut().find(|(seen, _)| *seen == kind) {
            Some((_, count)) => *count += 1,
            None => kinds.push((kind, 1)),
        }
    }
    let summary = kinds
        .iter()
        .map(|(kind, count)| format!("{count} {kind}"))
        .collect::<Vec<_>>()
        .join(", ");
    let status = if kernel::running_pid(path).is_some() {
        "live"
    } else {
        "off"
    };
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    emit(format_args!(
        "{name}: {} cells ({summary}), kernel {status}",
        notebook.cells().len()
    ));
    for (index, cell) in notebook.cells().iter().enumerate() {
        emit(format_args!("{}", outline_row(index, cell)));
    }
    Ok(0)
}

fn cmd_show(path: &Path, cells: &[String], no_output: bool, budget: Budget) -> anyhow::Result<u8> {
    let notebook = Notebook::load(path)?;
    for index in notebook.select(cells)? {
        let cell = &notebook.cells()[index];
        emit(format_args!("{}", cell_header(index, cell)));
        emit(format_args!("{}", source_of(cell)));
        if cell["cell_type"] == "code" && !no_output {
            emit(format_args!("── out ──"));
            emit(format_args!(
                "{}",
                render_outputs(
                    outputs_of(cell),
                    budget,
                    &mut image_sink(path, index.to_string())
                )?
            ));
        }
    }
    Ok(0)
}

fn cmd_grep(path: &Path, pattern: &str, include_outputs: bool) -> anyhow::Result<u8> {
    let notebook = Notebook::load(path)?;
    let pattern = Regex::new(pattern)?;
    let mut found = false;
    for (index, cell) in notebook.cells().iter().enumerate() {
        let mut texts = vec![(String::new(), source_of(cell))];
        if include_outputs {
            texts.extend(
                output_texts(outputs_of(cell))
                    .into_iter()
                    .map(|text| ("out ".to_owned(), text)),
            );
        }
        for (label, text) in texts {
            for (number, line) in clean(&text).lines().enumerate() {
                if pattern.is_match(line) {
                    found = true;
                    emit(format_args!(
                        "{index}:{label}{}: {}",
                        number + 1,
                        shorten(line.trim(), Some(GREP_WIDTH))
                    ));
                }
            }
        }
    }
    Ok(u8::from(!found))
}

fn cmd_errors(path: &Path, budget: Budget) -> anyhow::Result<u8> {
    let notebook = Notebook::load(path)?;
    let mut failing = 0;
    for (index, cell) in notebook.cells().iter().enumerate() {
        let errors: Vec<Value> = outputs_of(cell)
            .iter()
            .filter(|output| output["output_type"] == "error")
            .cloned()
            .collect();
        if !errors.is_empty() {
            failing += 1;
            emit(format_args!("{}", cell_header(index, cell)));
            emit(format_args!(
                "{}",
                render_outputs(&errors, budget, &mut image_sink(path, index.to_string()))?
            ));
        }
    }
    if failing == 0 {
        emit(format_args!("no errors"));
    }
    Ok(0)
}

fn cmd_set(path: &Path, spec: &str) -> anyhow::Result<u8> {
    let mut notebook = Notebook::load(path)?;
    let [index] = notebook.select(&[spec.to_owned()])?[..] else {
        bail!("set takes exactly one cell, {spec:?} selects several");
    };
    notebook.replace_source(index, &read_stdin()?);
    notebook.save()?;
    emit(format_args!("set {index}"));
    Ok(0)
}

fn cmd_add(path: &Path, at: Option<usize>, markdown: bool) -> anyhow::Result<u8> {
    let mut notebook = Notebook::load(path)?;
    let index = notebook.insert(
        at,
        if markdown { "markdown" } else { "code" },
        &read_stdin()?,
    );
    notebook.save()?;
    emit(format_args!("added {index}"));
    Ok(0)
}

fn cmd_rm(path: &Path, cells: &[String]) -> anyhow::Result<u8> {
    let mut notebook = Notebook::load(path)?;
    let indices = notebook.select(cells)?;
    notebook.remove(&indices);
    notebook.save()?;
    let listed = indices
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    emit(format_args!("removed {listed}"));
    Ok(0)
}

fn cmd_run(
    path: &Path,
    cells: &[String],
    budget: Budget,
    python: Option<&Path>,
) -> anyhow::Result<u8> {
    let mut notebook = Notebook::load(path)?;
    let code_cells: Vec<usize> = notebook
        .select(cells)?
        .into_iter()
        .filter(|&i| notebook.cells()[i]["cell_type"] == "code")
        .collect();
    let mut client = Client::connect(path, python)?;
    for index in code_cells {
        let execution = client.execute(&source_of(&notebook.cells()[index]), true)?;
        let cell = &mut notebook.cells_mut()[index];
        cell["outputs"] = Value::Array(execution.outputs.clone());
        cell["execution_count"] = execution.count.clone();
        notebook.save()?;
        let cell = &notebook.cells()[index];
        emit(format_args!("{}", cell_header(index, cell)));
        emit(format_args!(
            "{}",
            render_outputs(
                &execution.outputs,
                budget,
                &mut image_sink(path, index.to_string())
            )?
        ));
        if execution.failed() {
            return Ok(1);
        }
    }
    Ok(0)
}

fn cmd_eval(path: &Path, code: &str, budget: Budget, python: Option<&Path>) -> anyhow::Result<u8> {
    let code = if code == "-" {
        read_stdin()?
    } else {
        code.to_owned()
    };
    let execution = Client::connect(path, python)?.execute(&code, false)?;
    emit(format_args!(
        "{}",
        render_outputs(
            &execution.outputs,
            budget,
            &mut image_sink(path, "eval".to_owned())
        )?
    ));
    Ok(u8::from(execution.failed()))
}

fn cmd_kernel(path: &Path, action: KernelAction, python: Option<&Path>) -> anyhow::Result<u8> {
    match action {
        KernelAction::Status => match kernel::running_pid(path) {
            Some(pid) => emit(format_args!("live, pid {pid}")),
            None => emit(format_args!("off")),
        },
        KernelAction::Stop => emit(format_args!(
            "{}",
            if kernel::stop(path) {
                "stopped"
            } else {
                "was not running"
            }
        )),
        KernelAction::Restart => {
            kernel::stop(path);
            kernel::start(path, python)?;
            emit(format_args!("restarted, state is empty"));
        }
    }
    Ok(0)
}

fn dispatch(command: Command) -> anyhow::Result<u8> {
    match command {
        Command::Init { agent, project } => cmd_init(&agent, project),
        Command::Outline { notebook } => cmd_outline(&notebook),
        Command::Show {
            notebook,
            cells,
            no_output,
            output,
        } => cmd_show(&notebook, &cells, no_output, output.budget()),
        Command::Grep {
            notebook,
            pattern,
            outputs,
        } => cmd_grep(&notebook, &pattern, outputs),
        Command::Errors { notebook, output } => cmd_errors(&notebook, output.budget()),
        Command::Set { notebook, cell } => cmd_set(&notebook, &cell),
        Command::Add { notebook, at, md } => cmd_add(&notebook, at, md),
        Command::Rm { notebook, cells } => cmd_rm(&notebook, &cells),
        Command::Run {
            notebook,
            cells,
            output,
            kernel,
        } => cmd_run(&notebook, &cells, output.budget(), kernel.python.as_deref()),
        Command::Eval {
            notebook,
            code,
            output,
            kernel,
        } => cmd_eval(&notebook, &code, output.budget(), kernel.python.as_deref()),
        Command::Kernel {
            notebook,
            action,
            kernel,
        } => cmd_kernel(&notebook, action, kernel.python.as_deref()),
    }
}

fn main() -> ExitCode {
    match dispatch(Cli::parse().command) {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!("nblean: {error:#}");
            ExitCode::from(2)
        }
    }
}
