use std::path::PathBuf;
use std::sync::LazyLock;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use regex::Regex;
use serde_json::{Map, Value};

use crate::notebook::{joined, source_of};

static ANSI_ESCAPE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\x1b\[[0-?]*[ -/]*[@-~]").expect("static regex is valid"));
static LIBRARY_FRAME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"site-packages|\.pyx\b|\.pxi\b|[\\/]lib[\\/]python\d|[\\/]Lib[\\/]")
        .expect("static regex is valid")
});
const TRACEBACK_HEADER: &str = "Traceback (most recent call last)";
const HEAD_WIDTH: usize = 72;

pub type ImageSink<'a> = dyn FnMut(usize, &str, &[u8]) -> anyhow::Result<PathBuf> + 'a;

#[derive(Clone, Copy)]
pub struct Budget {
    pub lines: Option<usize>,
    pub width: Option<usize>,
}

impl Budget {
    pub const FULL: Self = Self {
        lines: None,
        width: None,
    };

    #[must_use]
    pub const fn clipped(lines: usize) -> Self {
        Self {
            lines: Some(lines),
            width: Some(160),
        }
    }
}

#[must_use]
pub fn shorten(line: &str, width: Option<usize>) -> String {
    match width {
        Some(width) if line.chars().count() > width => {
            let mut short: String = line.chars().take(width - 1).collect();
            short.push('…');
            short
        }
        _ => line.to_owned(),
    }
}

#[must_use]
pub fn clean(text: &str) -> String {
    let stripped = ANSI_ESCAPE.replace_all(text, "");
    stripped
        .split('\n')
        .map(|line| {
            line.trim_end_matches('\r')
                .rsplit('\r')
                .next()
                .unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[must_use]
pub fn clip(text: &str, budget: Budget, tail_only: bool) -> String {
    let cleaned = clean(text);
    let lines: Vec<String> = cleaned
        .trim_end_matches('\n')
        .split('\n')
        .map(|line| shorten(line, budget.width))
        .collect();
    let Some(limit) = budget.lines.filter(|&limit| lines.len() > limit) else {
        return lines.join("\n");
    };
    let head = if tail_only { 0 } else { limit / 2 };
    let tail = limit - head;
    let mut kept = lines[..head].to_vec();
    kept.push(format!("… {} lines omitted …", lines.len() - limit));
    kept.extend_from_slice(&lines[lines.len() - tail..]);
    kept.join("\n")
}

#[must_use]
pub fn compact_traceback(traceback: &[String]) -> String {
    let entries: Vec<String> = traceback.iter().map(|entry| clean(entry)).collect();
    let start = entries
        .iter()
        .rposition(|entry| entry.contains(TRACEBACK_HEADER))
        .unwrap_or(0);
    let Some((exception, frames)) = entries[start..].split_last() else {
        return String::new();
    };
    let kept: Vec<&String> = frames
        .iter()
        .filter(|frame| !LIBRARY_FRAME.is_match(frame.split('\n').next().unwrap_or_default()))
        .collect();
    let hidden = entries.len() - 1 - kept.len();
    let mut parts: Vec<String> = kept.into_iter().cloned().collect();
    if hidden > 0 {
        parts.push(format!("… {hidden} library or chained frames omitted …"));
    }
    parts.push(exception.clone());
    parts.join("\n")
}

#[must_use]
pub fn line_count(text: &str) -> usize {
    clean(text).trim_end_matches('\n').matches('\n').count() + 1
}

fn image_mime(data: &Map<String, Value>) -> Option<&str> {
    data.keys()
        .map(String::as_str)
        .find(|mime| mime.starts_with("image/"))
}

#[must_use]
pub fn image_suffix(mime: &str) -> &'static str {
    match mime {
        "image/png" => ".png",
        "image/jpeg" => ".jpg",
        "image/svg+xml" => ".svg",
        "image/gif" => ".gif",
        _ => ".bin",
    }
}

fn image_bytes(mime: &str, payload: &Value) -> anyhow::Result<Vec<u8>> {
    let text = joined(payload);
    if mime == "image/svg+xml" {
        return Ok(text.into_bytes());
    }
    let compact: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    Ok(STANDARD.decode(compact)?)
}

fn traceback_of(output: &Value) -> Vec<String> {
    output["traceback"]
        .as_array()
        .map(|frames| {
            frames
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn render_error(output: &Value, budget: Budget) -> String {
    let traceback = traceback_of(output);
    if traceback.is_empty() {
        return format!(
            "{}: {}",
            output["ename"].as_str().unwrap_or_default(),
            output["evalue"].as_str().unwrap_or_default()
        );
    }
    let text = if budget.lines.is_none() {
        traceback.join("\n")
    } else {
        compact_traceback(&traceback)
    };
    clip(&text, budget, true)
}

pub fn render_output(
    position: usize,
    output: &Value,
    budget: Budget,
    sink: &mut ImageSink<'_>,
) -> anyhow::Result<String> {
    match output["output_type"].as_str() {
        Some("stream") => return Ok(clip(&joined(&output["text"]), budget, false)),
        Some("error") => return Ok(render_error(output, budget)),
        _ => {}
    }
    let Some(data) = output["data"].as_object() else {
        return Ok(format!(
            "[{}]",
            output["output_type"].as_str().unwrap_or("unknown output")
        ));
    };
    if let Some(mime) = image_mime(data) {
        let path = sink(position, mime, &image_bytes(mime, &data[mime])?)?;
        return Ok(format!("[{mime} → {}]", path.display()));
    }
    if let Some(text) = data.get("text/plain") {
        return Ok(clip(&joined(text), budget, false));
    }
    Ok(format!(
        "[{}]",
        data.keys().cloned().collect::<Vec<_>>().join(", ")
    ))
}

pub fn render_outputs(
    outputs: &[Value],
    budget: Budget,
    sink: &mut ImageSink<'_>,
) -> anyhow::Result<String> {
    let rendered = outputs
        .iter()
        .enumerate()
        .map(|(position, output)| render_output(position, output, budget, sink))
        .collect::<anyhow::Result<Vec<_>>>()?;
    Ok(if rendered.is_empty() {
        "(no output)".to_owned()
    } else {
        rendered.join("\n")
    })
}

#[must_use]
pub fn summarize(outputs: &[Value]) -> String {
    let mut text_lines = 0;
    let mut marks: Vec<(String, usize)> = Vec::new();
    let mut mark = |label: String| match marks.iter_mut().find(|(seen, _)| *seen == label) {
        Some((_, count)) => *count += 1,
        None => marks.push((label, 1)),
    };
    for output in outputs {
        let data = output["data"].as_object();
        match output["output_type"].as_str() {
            Some("error") => mark(format!(
                "ERR {}",
                output["ename"].as_str().unwrap_or_default()
            )),
            Some("stream") => text_lines += line_count(&joined(&output["text"])),
            _ if data.is_some_and(|data| image_mime(data).is_some()) => mark("img".to_owned()),
            _ if let Some(text) = data.and_then(|data| data.get("text/plain")) => {
                text_lines += line_count(&joined(text));
            }
            _ => mark("other".to_owned()),
        }
    }
    let mut parts = if text_lines > 0 {
        vec![format!("{text_lines}L")]
    } else {
        Vec::new()
    };
    parts.extend(marks.into_iter().map(|(label, count)| {
        if count == 1 {
            label
        } else {
            format!("{label}×{count}")
        }
    }));
    parts.join(" ")
}

#[must_use]
pub fn outline_row(index: usize, cell: &Value) -> String {
    let source = source_of(cell);
    let lines: Vec<&str> = source
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    let head = lines
        .first()
        .map(|line| shorten(line, Some(HEAD_WIDTH)))
        .unwrap_or_default();
    let extra = if lines.len() > 1 {
        format!(" +{}L", lines.len() - 1)
    } else {
        String::new()
    };
    let mut suffix = String::new();
    let tag = match cell["cell_type"].as_str().unwrap_or_default() {
        "code" => {
            if cell["execution_count"].is_null() {
                "  → not run".clone_into(&mut suffix);
            } else {
                let summary = summarize(cell["outputs"].as_array().map_or(&[], Vec::as_slice));
                if !summary.is_empty() {
                    suffix = format!("  → {summary}");
                }
            }
            "code"
        }
        "markdown" => "md",
        kind => kind,
    };
    format!("{index:>3} {tag:<5} {head}{extra}{suffix}")
}

#[must_use]
pub fn cell_header(index: usize, cell: &Value) -> String {
    let mut fields = vec![
        index.to_string(),
        cell["cell_type"].as_str().unwrap_or_default().to_owned(),
    ];
    if let Some(id) = cell["id"].as_str() {
        fields.push(format!("id={id}"));
    }
    if let Some(count) = cell["execution_count"].as_u64() {
        fields.push(format!("exec={count}"));
    }
    format!("── {} ──", fields.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clip_keeps_head_and_tail() {
        let text = (0..100)
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(
            clip(
                &text,
                Budget {
                    lines: Some(4),
                    width: None
                },
                false
            ),
            "0\n1\n… 96 lines omitted …\n98\n99"
        );
    }

    #[test]
    fn clean_resolves_carriage_returns_and_ansi() {
        assert_eq!(
            clip(
                "10%\r50%\r100%\n\x1b[1mdone\x1b[0m",
                Budget::clipped(20),
                false
            ),
            "100%\ndone"
        );
    }

    #[test]
    fn traceback_hides_library_and_chained_frames() {
        let traceback: Vec<String> = [
            "KeyError   Traceback (most recent call last)",
            "File x.pyx:1",
            "KeyError: 'presure'",
            "KeyError   Traceback (most recent call last)",
            "Cell In[3], line 1\n----> 1 df['presure']",
            "File /venv/lib/python3.13/site-packages/pandas/core/frame.py:4378, in getitem\n  ...",
            "KeyError: 'presure'",
        ]
        .map(str::to_owned)
        .to_vec();
        assert_eq!(
            compact_traceback(&traceback),
            "KeyError   Traceback (most recent call last)\nCell In[3], line 1\n----> 1 df['presure']\n… 4 library or chained frames omitted …\nKeyError: 'presure'"
        );
    }
}
