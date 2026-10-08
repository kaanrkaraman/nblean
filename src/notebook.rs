use std::fs;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use anyhow::{Context, bail};
use regex::Regex;
use serde_json::{Value, json};

static CELL_RANGE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(\d+)-(\d*)$").expect("static regex is valid"));

#[must_use]
pub fn joined(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts.iter().filter_map(Value::as_str).collect(),
        _ => String::new(),
    }
}

#[must_use]
pub fn source_of(cell: &Value) -> String {
    joined(&cell["source"])
}

fn lines_of(text: &str) -> Value {
    let body = text.strip_suffix('\n').unwrap_or(text);
    Value::Array(body.split_inclusive('\n').map(Value::from).collect())
}

pub struct Notebook {
    pub path: PathBuf,
    data: Value,
}

impl Notebook {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text = fs::read_to_string(path).with_context(|| path.display().to_string())?;
        let data: Value =
            serde_json::from_str(&text).with_context(|| path.display().to_string())?;
        if !data["cells"].is_array() {
            bail!("{}: not a notebook, no cells array", path.display());
        }
        Ok(Self {
            path: path.to_owned(),
            data,
        })
    }

    #[must_use]
    pub fn cells(&self) -> &[Value] {
        self.data["cells"].as_array().map_or(&[], Vec::as_slice)
    }

    pub fn cells_mut(&mut self) -> &mut Vec<Value> {
        self.data["cells"]
            .as_array_mut()
            .expect("load verified the cells array")
    }

    fn has_cell_ids(&self) -> bool {
        let major = self.data["nbformat"].as_u64().unwrap_or(4);
        let minor = self.data["nbformat_minor"].as_u64().unwrap_or(0);
        (major, minor) >= (4, 5)
    }

    pub fn save(&self) -> anyhow::Result<()> {
        let mut buffer = Vec::new();
        let formatter = serde_json::ser::PrettyFormatter::with_indent(b" ");
        let mut serializer = serde_json::Serializer::with_formatter(&mut buffer, formatter);
        serde::Serialize::serialize(&self.data, &mut serializer)?;
        buffer.push(b'\n');
        let name = self
            .path
            .file_name()
            .context("notebook path has no file name")?;
        let staging = self
            .path
            .with_file_name(format!(".{}.nblean", name.to_string_lossy()));
        fs::write(&staging, buffer)?;
        fs::rename(&staging, &self.path)?;
        Ok(())
    }

    pub fn select(&self, specs: &[String]) -> anyhow::Result<Vec<usize>> {
        let mut picked: Vec<usize> = Vec::new();
        for spec in specs {
            for index in self.resolve(spec)? {
                if !picked.contains(&index) {
                    picked.push(index);
                }
            }
        }
        Ok(picked)
    }

    fn resolve(&self, spec: &str) -> anyhow::Result<Vec<usize>> {
        let count = self.cells().len();
        if spec == "all" {
            return Ok((0..count).collect());
        }
        if !spec.is_empty() && spec.bytes().all(|byte| byte.is_ascii_digit()) {
            let index: usize = spec.parse()?;
            if index >= count {
                bail!("cell {index} out of range, notebook has {count} cells");
            }
            return Ok(vec![index]);
        }
        if let Some(captures) = CELL_RANGE.captures(spec) {
            let start: usize = captures[1].parse()?;
            let stop = match &captures[2] {
                "" => count,
                end => (end.parse::<usize>()? + 1).min(count),
            };
            return Ok((start..stop).collect());
        }
        let hits: Vec<usize> = self
            .cells()
            .iter()
            .enumerate()
            .filter(|(_, cell)| cell["id"].as_str().is_some_and(|id| id.starts_with(spec)))
            .map(|(index, _)| index)
            .collect();
        if hits.len() != 1 {
            bail!("cell id {spec:?} matched {} cells", hits.len());
        }
        Ok(hits)
    }

    pub fn replace_source(&mut self, index: usize, text: &str) {
        let cell = &mut self.cells_mut()[index];
        cell["source"] = lines_of(text);
        if cell["cell_type"] == "code" {
            cell["execution_count"] = Value::Null;
            cell["outputs"] = json!([]);
        }
    }

    pub fn insert(&mut self, index: Option<usize>, kind: &str, text: &str) -> usize {
        let mut cell = json!({"cell_type": kind, "metadata": {}, "source": lines_of(text)});
        if self.has_cell_ids() {
            cell["id"] = Value::from(uuid::Uuid::new_v4().simple().to_string()[..8].to_owned());
        }
        if kind == "code" {
            cell["execution_count"] = Value::Null;
            cell["outputs"] = json!([]);
        }
        let cells = self.cells_mut();
        let position = index.map_or(cells.len(), |index| index.min(cells.len()));
        cells.insert(position, cell);
        position
    }

    pub fn remove(&mut self, indices: &[usize]) {
        let mut sorted = indices.to_vec();
        sorted.sort_unstable_by(|a, b| b.cmp(a));
        let cells = self.cells_mut();
        for index in sorted {
            cells.remove(index);
        }
    }
}
