use std::{collections::HashSet, sync::LazyLock, time::Duration};

use anyhow::{Context, Result, ensure};
use regex::Regex;
use reqwest::Client;
use scraper::{Html, Selector};
use serde::Deserialize;
use time::{Date, OffsetDateTime, macros::format_description};

use super::{Evidence, Metric, Value};

const OPENAI: &str = "https://developers.openai.com/api/docs/pricing.md";
const ANTHROPIC: &str = "https://platform.claude.com/docs/en/about-claude/pricing.md";
const GOOGLE: &str = "https://aistudio.google.com/docs/pricing.md";
const AA: &str = "https://artificialanalysis.ai/leaderboards/models";
const EPOCH: &str = "https://epoch.ai/data/eci_scores.csv";
const VALS: &str = "https://www.vals.ai/benchmarks/terminal-bench-4";
const ARENA: &str = "https://huggingface.co/datasets/lmarena-ai/leaderboard-dataset";
const ARENA_DATA: &str = "https://datasets-server.huggingface.co/rows";
const MAX_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug)]
pub(super) struct SourceRow {
    pub label: String,
    pub data: Evidence,
}

fn row(label: &str, metric: Metric, value: Value, url: &str, edition: &str) -> SourceRow {
    SourceRow {
        label: label.to_owned(),
        data: Evidence {
            metric,
            value,
            url: url.to_owned(),
            label: label.to_owned(),
            edition: edition.to_owned(),
            checked_at: Some(OffsetDateTime::now_utc().unix_timestamp()),
        },
    }
}

#[derive(Debug, Clone, Copy)]
enum Source {
    OpenAi,
    Anthropic,
    Google,
    ArtificialAnalysis,
    Epoch,
    Arena,
    Vals,
}

impl Source {
    fn label(self) -> &'static str {
        match self {
            Self::OpenAi => "OpenAI pricing",
            Self::Anthropic => "Anthropic pricing",
            Self::Google => "Google pricing",
            Self::ArtificialAnalysis => "Artificial Analysis",
            Self::Epoch => "Epoch General ECI",
            Self::Arena => "Text Arena",
            Self::Vals => "Vals Terminal-Bench 4.0",
        }
    }

    async fn load(self, client: &Client) -> Result<Vec<SourceRow>> {
        let rows = match self {
            Self::OpenAi => pricing_table(&get(client, OPENAI).await?, OPENAI),
            Self::Anthropic => pricing_table(&get(client, ANTHROPIC).await?, ANTHROPIC),
            Self::Google => google_prices(
                &get(client, GOOGLE).await?,
                OffsetDateTime::now_utc().date(),
            ),
            Self::ArtificialAnalysis => artificial_analysis(client).await?,
            Self::Epoch => epoch(&get(client, EPOCH).await?)?,
            Self::Arena => arena(client).await?,
            Self::Vals => vals(&get(client, VALS).await?),
        };
        ensure!(
            !rows.is_empty(),
            "The source did not return comparable rows"
        );
        Ok(rows)
    }
}

pub(super) async fn collect() -> Result<(Vec<SourceRow>, Vec<String>)> {
    let client = Client::builder()
        .tls_backend_native()
        .user_agent("OmniAgent/0.1 (public model catalog refresh)")
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(15))
        .build()?;
    let mut tasks = tokio::task::JoinSet::new();
    for source in [
        Source::OpenAi,
        Source::Anthropic,
        Source::Google,
        Source::ArtificialAnalysis,
        Source::Epoch,
        Source::Arena,
        Source::Vals,
    ] {
        let client = client.clone();
        tasks.spawn(async move { (source, source.load(&client).await) });
    }
    let mut rows = Vec::new();
    let mut issues = Vec::new();
    while let Some(result) = tasks.join_next().await {
        match result? {
            (_, Ok(found)) => rows.extend(found),
            (source, Err(_)) => issues.push(format!("{} was unavailable.", source.label())),
        }
    }
    Ok((rows, issues))
}

async fn get(client: &Client, url: &str) -> Result<String> {
    let mut response = client.get(url).send().await?.error_for_status()?;
    ensure!(response.status().is_success(), "Unexpected source redirect");
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        ensure!(
            bytes.len() + chunk.len() <= MAX_BYTES,
            "Source response is too large"
        );
        bytes.extend(chunk);
    }
    Ok(String::from_utf8(bytes)?)
}

fn table_rows(body: &str, table_headers: &[&str]) -> Vec<(Vec<String>, Option<String>)> {
    let document = Html::parse_document(body);
    let tables = Selector::parse("table").unwrap();
    let rows = Selector::parse("tr").unwrap();
    let cells = Selector::parse("th, td").unwrap();
    let links = Selector::parse("a[href]").unwrap();
    let text = |element: scraper::ElementRef<'_>| {
        element
            .text()
            .flat_map(str::split_whitespace)
            .collect::<Vec<_>>()
            .join(" ")
    };
    document
        .select(&tables)
        .filter(|table| {
            table.select(&rows).any(|line| {
                let headers: Vec<_> = line.select(&cells).map(text).collect();
                table_headers
                    .iter()
                    .all(|header| headers.iter().any(|cell| cell == header))
            })
        })
        .flat_map(|table| table.select(&rows))
        .map(|line| {
            let link = line
                .select(&links)
                .filter_map(|anchor| anchor.attr("href"))
                .find(|link| link.starts_with("/models/"))
                .map(str::to_owned);
            (line.select(&cells).map(text).collect(), link)
        })
        .collect()
}

fn amount(cell: &str) -> Option<f64> {
    let value: f64 = cell
        .trim()
        .strip_prefix('$')?
        .split(|ch: char| !(ch.is_ascii_digit() || ch == '.' || ch == ','))
        .next()?
        .replace(',', "")
        .parse()
        .ok()?;
    (value.is_finite() && (0.0..=1_000_000.0).contains(&value)).then_some(value)
}

fn pricing_table(body: &str, url: &str) -> Vec<SourceRow> {
    let mut rows = Vec::new();
    let mut model_labels = HashSet::new();
    let mut columns = None;
    let mut standard = false;
    for line in body.lines() {
        if line.starts_with('#') {
            columns = None;
            standard = line.trim_start_matches('#').trim() == "Standard pricing data";
        }
        if !line.starts_with('|') {
            if url == ANTHROPIC && !rows.is_empty() {
                break;
            }
            continue;
        }
        let cells: Vec<_> = line
            .trim()
            .trim_matches('|')
            .split('|')
            .map(str::trim)
            .collect();
        if cells.first() == Some(&"Model") {
            if url == ANTHROPIC {
                standard = cells.contains(&"Base input tokens");
            }
            let input = cells
                .iter()
                .position(|v| matches!(*v, "Input" | "Base input tokens" | "Short context input"));
            let output = cells
                .iter()
                .position(|v| matches!(*v, "Output" | "Output tokens" | "Short context output"));
            columns = input.zip(output);
            continue;
        }
        if !standard {
            continue;
        }
        let Some((input, output)) = columns else {
            continue;
        };
        let label = cells[0]
            .split(" (")
            .next()
            .unwrap_or("")
            .trim_matches('\u{60}');
        if label.is_empty() || cells[0].contains("over 100,000") {
            continue;
        }
        if let Some((input, output)) = cells
            .get(input)
            .and_then(|v| amount(v))
            .zip(cells.get(output).and_then(|v| amount(v)))
            && model_labels.insert(label.to_owned())
        {
            rows.push(row(
                label,
                Metric::Price,
                Value::Rates { input, output },
                url,
                "text-on-demand",
            ));
        }
    }
    rows
}

fn google_prices(body: &str, today: Date) -> Vec<SourceRow> {
    static DATE_TEXT: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?:through|starting) ([A-Z][a-z]+ \d{1,2}, \d{4})").unwrap());
    let date_format = format_description!("[month repr:long] [day padding:none], [year]");
    let price = |block: &str| {
        block
            .lines()
            .filter_map(|line| {
                let line = line.trim().trim_start_matches("* ");
                let value = amount(line)?;
                if let Some(parts) = DATE_TEXT.captures(line) {
                    let date = Date::parse(parts.get(1)?.as_str(), date_format).ok()?;
                    let effective = if line.contains("through ") {
                        today <= date
                    } else {
                        today >= date
                    };
                    if !effective {
                        return None;
                    }
                }
                Some(value)
            })
            .next()
    };
    body.split("\n## ")
        .filter_map(|section| {
            let title = section.lines().next()?;
            let (_, tail) = title.split_once("id=\"gemini-")?;
            let (suffix, _) = tail.split_once('"')?;
            let label = format!("gemini-{suffix}");
            let (_, standard) = section.split_once("{% tab title=\"Standard\" %}")?;
            let (standard, _) = standard.split_once("{% /tab %}")?;
            let field = |name: &str| standard.split_once(name)?.1.split("---").next();
            let input = price(field("* Input price")?)?;
            let output = price(field("* Output price")?)?;
            Some(row(
                &label,
                Metric::Price,
                Value::Rates { input, output },
                &format!("https://aistudio.google.com/docs/pricing#{label}"),
                "text-on-demand",
            ))
        })
        .collect()
}

fn score(text: &str, max: f64) -> Option<Value> {
    let number: f64 = text.trim_end_matches('%').parse().ok()?;
    (number.is_finite() && (0.0..=max).contains(&number)).then_some(Value::Score(number))
}

async fn artificial_analysis(client: &Client) -> Result<Vec<SourceRow>> {
    static EDITION: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"Intelligence Index v(\d+(?:\.\d+)+)").unwrap());
    let body = get(client, AA).await?;
    let edition = EDITION
        .captures(&body)
        .and_then(|m| m.get(1))
        .context("Missing Artificial Analysis Index version")?
        .as_str();
    let mut rows = Vec::new();
    let mut gemini = HashSet::new();
    for (cells, link) in table_rows(&body, &["Model", "Artificial Analysis Intelligence Index"]) {
        if let [label, value, ..] = cells.as_slice()
            && let Some(value) = score(value, 100.0)
        {
            rows.push(row(label, Metric::ArtificialAnalysis, value, AA, edition));
            if label.starts_with("Gemini ")
                && label.ends_with(" Xhigh")
                && let Some(link) = link
                && link.strip_prefix("/models/").is_some_and(|slug| {
                    !slug.is_empty()
                        && slug
                            .bytes()
                            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                })
            {
                gemini.insert(link);
            }
        }
    }
    let mut tasks = tokio::task::JoinSet::new();
    for link in gemini {
        let client = client.clone();
        let edition = edition.to_owned();
        tasks.spawn(async move {
            let url = format!("https://artificialanalysis.ai{link}");
            let Ok(page) = get(&client, &url).await else {
                return Vec::new();
            };
            if EDITION
                .captures(&page)
                .and_then(|m| m.get(1))
                .is_some_and(|m| m.as_str() != edition)
            {
                return Vec::new();
            }
            table_rows(&page, &["Model Name", "Intelligence"])
                .into_iter()
                .filter_map(|(cells, _)| {
                    let [label, _, value, ..] = cells.as_slice() else {
                        return None;
                    };
                    Some(row(
                        label,
                        Metric::ArtificialAnalysis,
                        score(value, 100.0)?,
                        &url,
                        &edition,
                    ))
                })
                .collect::<Vec<_>>()
        });
    }
    while let Some(found) = tasks.join_next().await {
        rows.extend(found?);
    }
    Ok(rows)
}

fn vals(body: &str) -> Vec<SourceRow> {
    if !body.contains("Terminal-Bench 4.0") {
        return Vec::new();
    }
    table_rows(body, &["Model", "Harness", "Score"])
        .into_iter()
        .filter_map(|(cells, _)| {
            let [_, label, harness, value, ..] = cells.as_slice() else {
                return None;
            };
            if !harness.eq_ignore_ascii_case("Mini-SWE-agent") || !value.ends_with('%') {
                return None;
            }
            Some(row(
                label,
                Metric::TerminalBench,
                score(value, 100.0)?,
                VALS,
                "4.0 Mini-SWE-agent",
            ))
        })
        .collect()
}

fn epoch(body: &str) -> Result<Vec<SourceRow>> {
    #[derive(Deserialize)]
    struct Entry {
        #[serde(rename = "Display name")]
        name: String,
        eci: f64,
    }
    let mut rows = Vec::new();
    for item in csv::Reader::from_reader(body.as_bytes()).deserialize::<Entry>() {
        let Ok(item) = item else { continue };
        if item.eci.is_finite() {
            rows.push(row(
                &item.name,
                Metric::Epoch,
                Value::Score(item.eci),
                EPOCH,
                "General ECI",
            ));
        }
    }
    Ok(rows)
}

async fn arena(client: &Client) -> Result<Vec<SourceRow>> {
    #[derive(Deserialize)]
    struct Page {
        rows: Vec<Record>,
        num_rows_total: usize,
    }
    #[derive(Deserialize)]
    struct Record {
        row: Entry,
    }
    #[derive(Deserialize)]
    struct Entry {
        model_name: String,
        rating: f64,
        category: String,
    }
    let mut rows = Vec::new();
    for offset in (0..2000).step_by(100) {
        let url = format!(
            "{ARENA_DATA}?dataset=lmarena-ai%2Fleaderboard-dataset&config=text_style_control&split=latest&offset={offset}&length=100"
        );
        let page: Page = serde_json::from_str(&get(client, &url).await?)?;
        if page.rows.is_empty() {
            break;
        }
        let mut other_category = false;
        for record in page.rows {
            let entry = record.row;
            if entry.category != "overall" {
                other_category = true;
            } else if entry.rating.is_finite() && entry.rating > 0.0 {
                rows.push(row(
                    &entry.model_name,
                    Metric::TextArena,
                    Value::Score(entry.rating.round()),
                    ARENA,
                    "Text style control",
                ));
            }
        }
        if other_category || offset + 100 >= page.num_rows_total {
            break;
        }
    }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parsing_uses_only_standard_pricing_and_the_current_gemini_rate() {
        let openai = "### Standard pricing data\n| Model | Short context input | Short context output | Cached |\n| gpt-6.2-sol | $2.00 | $10 | $0.10 |\n### Flex pricing data\n| Model | Input | Output |\n| gpt-6.2-sol | $1 | $5 |";
        let rows = pricing_table(openai, OPENAI);
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].data.value,
            Value::Rates {
                input: 2.0,
                output: 10.0
            }
        );

        let google = "Intro\n## Gemini 4 Argon {% id=\"gemini-4-argon\" %}\n{% tab title=\"Standard\" %}\n* Input price\n* $2 through November 30, 2026.\n$4 starting December 1, 2026.\n---\n* Output price\n* $10 through November 30, 2026.\n$20 starting December 1, 2026.\n---\n{% /tab %}";
        assert_eq!(
            google_prices(google, time::macros::date!(2026 - 11 - 17))[0]
                .data
                .value,
            Value::Rates {
                input: 2.0,
                output: 10.0
            }
        );
        assert_eq!(
            google_prices(google, time::macros::date!(2026 - 12 - 01))[0]
                .data
                .value,
            Value::Rates {
                input: 4.0,
                output: 20.0
            }
        );
    }
}
