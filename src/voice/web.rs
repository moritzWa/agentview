//! Web search for the voice: one Exa query, a few results.

use std::io::Write;
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

const RESULTS: usize = 5;
const SNIPPET_CHARS: usize = 200;

/// Sends one query to Exa and returns `[{title, url, snippet}]`. The key goes
/// through curl's stdin so it never appears in the process list.
pub fn search(query: &str, key: &str) -> Result<Vec<Value>> {
    let body = json!({
        "query": query,
        "type": "auto",
        "numResults": RESULTS,
        "contents": {"highlights": {"maxCharacters": 300}},
    });
    let mut child = Command::new("curl")
        .args([
            "-sS",
            "--max-time",
            "20",
            "-H",
            "@-",
            "-H",
            "Content-Type: application/json",
            "--data",
            &body.to_string(),
            "-w",
            "\n%{http_code}",
            "https://api.exa.ai/search",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("curl is needed for web search")?;
    writeln!(child.stdin.take().context("no stdin")?, "x-api-key: {key}")?;
    let output = child.wait_with_output()?;
    let text = String::from_utf8_lossy(&output.stdout).into_owned();
    let (body, status) = text.rsplit_once('\n').unwrap_or((&text, ""));
    match status.trim() {
        "200" => parse(body),
        "401" | "403" => bail!("Exa rejected the search key"),
        "" | "000" => bail!(
            "could not reach Exa: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ),
        other => bail!("Exa answered {other}"),
    }
}

/// The results of an Exa answer, each with a one-line snippet.
fn parse(body: &str) -> Result<Vec<Value>> {
    let answer: Value = serde_json::from_str(body).context("Exa sent something unreadable")?;
    Ok(answer["results"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter_map(|result| {
            let url = result["url"].as_str()?;
            let title = one_line(result["title"].as_str().unwrap_or(url));
            let snippet = result["highlights"]
                .as_array()
                .and_then(|highlights| highlights.first())
                .and_then(Value::as_str)
                .map(one_line)
                .unwrap_or_default();
            Some(json!({"title": title, "url": url, "snippet": clip(&snippet)}))
        })
        .collect())
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn clip(text: &str) -> String {
    if text.chars().count() <= SNIPPET_CHARS {
        return text.to_owned();
    }
    let head: String = text.chars().take(SNIPPET_CHARS).collect();
    format!("{head}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn results_become_title_url_and_a_one_line_snippet() {
        let long = "word ".repeat(100);
        let body = json!({"results": [
            {"title": "Ratatui\n releases", "url": "https://a.test/r", "highlights": ["line one\n\nline two", "second"]},
            {"url": "https://b.test/x", "highlights": []},
            {"title": "no url"},
            {"title": "Long", "url": "https://c.test", "highlights": [long]},
        ]});
        let found = parse(&body.to_string()).unwrap();
        assert_eq!(found.len(), 3);
        assert_eq!(found[0]["title"], "Ratatui releases");
        assert_eq!(found[0]["snippet"], "line one line two");
        assert_eq!(found[1]["title"], "https://b.test/x");
        assert_eq!(found[1]["snippet"], "");
        assert!(found[2]["snippet"].as_str().unwrap().chars().count() <= SNIPPET_CHARS + 1);
    }

    #[test]
    fn an_answer_without_results_is_an_empty_list() {
        assert!(parse("{}").unwrap().is_empty());
        assert!(parse("<html>").is_err());
    }
}
