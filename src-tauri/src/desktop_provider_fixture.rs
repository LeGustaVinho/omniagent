use std::{
    env,
    ffi::OsString,
    fs,
    io::{self, BufRead, Write},
    path::Path,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};

pub const APP_IDENTIFIER: &str = "io.github.legustavinho.omniagent.desktop-tests";
const PROFILE_ENV: &str = "OMNIAGENT_DESKTOP_TEST_RUN_ID";
const MODEL: &str = "desktop-fixture";
const SESSION: &str = "desktop-fixture-session";
const TURN: &str = "desktop-fixture-turn";
const RELEASE: &str = "release-desktop-jobs";
const COMPOSER_PROMPT: &str = "Apply the selected local skill and plugin export.";
const COMPOSER_PLUGIN: &str = "desktop-formatting@fixture-marketplace";

pub fn isolated_identifier() -> Result<Option<String>> {
    let Some(run_id) = env::var_os(PROFILE_ENV) else {
        return Ok(None);
    };
    let run_id = run_id.to_str().context("Invalid desktop test run ID")?;
    ensure!(
        run_id.len() == 32
            && run_id
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "The desktop test run ID must contain 32 lowercase hexadecimal digits"
    );
    Ok(Some(format!("{APP_IDENTIFIER}.run-{run_id}")))
}

pub fn run_from_args() -> Option<i32> {
    let args = env::args_os().skip(1).collect::<Vec<OsString>>();
    let mut command = args.as_slice();
    while let [flag, _, rest @ ..] = command
        && flag == "-c"
    {
        command = rest;
    }
    match command {
        [version] if version == "--version" => println!("codex-cli 0.169.0-alpha.11.1"),
        [login, status] if login == "login" && status == "status" => {
            println!("Logged in to the desktop fixture")
        }
        [plugin, list, json] if plugin == "plugin" && list == "list" && json == "--json" => {
            println!("{}", json!({"installed":[]}))
        }
        [server, ..] if server == "app-server" => {
            if let Err(error) = run() {
                eprintln!("Desktop provider fixture: {error:#}");
                return Some(1);
            }
        }
        _ => return None,
    }
    Some(0)
}

fn run() -> Result<()> {
    let root = env::current_dir()?;
    let mut stdout = io::stdout().lock();
    for line in io::stdin().lock().lines() {
        let request: Value = serde_json::from_str(&line?)?;
        let result = match request["method"].as_str() {
            Some("initialize") => json!({"userAgent":"omniagent-desktop-fixture"}),
            Some("initialized") => continue,
            Some("thread/start") => json!({
                "thread":{"id":SESSION}, "cwd":root, "model":MODEL, "sandbox":{"type":"readOnly"}
            }),
            Some("model/list") => json!({"data":[{"id":MODEL,"displayName":"Desktop fixture"}]}),
            Some("config/read") => {
                json!({"config":{"sandbox_mode":"read-only","approval_policy":"never"}})
            }
            Some("skills/list") => composer_catalog(&request),
            Some("turn/start") => {
                writeln!(
                    stdout,
                    "{}",
                    json!({"id":request["id"],"result":{"turn":{"id":TURN}}})
                )?;
                stdout.flush()?;
                let text = request
                    .pointer("/params/input/0/text")
                    .and_then(Value::as_str)
                    .context("the fixture turn did not receive a text prompt")?;
                if text.contains("The user answered questions from the original turn.") {
                    ensure!(
                        text.contains("\"format\":\"Markdown\"")
                            && text.contains("\"summary\":\"Yes\""),
                        "the linked turn did not receive both typed answers"
                    );
                    finished(
                        &mut stdout,
                        "completed",
                        Some("The linked planning turn received both answers."),
                    )?;
                } else if let Some(kind) = text.strip_prefix("Ask desktop question ") {
                    ensure!(
                        matches!(kind, "live" | "default" | "custom"),
                        "unknown question fixture"
                    );
                    question(
                        &mut stdout,
                        71,
                        "format",
                        "Which format?",
                        &["Markdown", "JSON"],
                    )?;
                    if kind != "live" {
                        wait_for_file(&root.join(format!("advance-{kind}-question")))?;
                        question(
                            &mut stdout,
                            72,
                            "summary",
                            "Include a summary?",
                            &["Yes", "No"],
                        )?;
                    }
                } else if text.starts_with(COMPOSER_PROMPT) {
                    verify_composer_input(&root, &request, text)?;
                    finished(
                        &mut stdout,
                        "completed",
                        Some("Received both native skill inputs and the selected chat excerpts."),
                    )?;
                } else {
                    finished(&mut stdout, "completed", Some(complete(&root, text)?))?;
                }
                continue;
            }
            Some("turn/interrupt") => {
                writeln!(stdout, "{}", json!({"id":request["id"],"result":{}}))?;
                finished(&mut stdout, "interrupted", None)?;
                continue;
            }
            None if request["id"] == 71 => {
                ensure!(
                    request.pointer("/result/answers/format/answers/0") == Some(&json!("Markdown")),
                    "the live question missed its typed answer"
                );
                let mut receipt = fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(root.join("desktop-live-question-reply"))?;
                writeln!(receipt, "Markdown")?;
                finished(
                    &mut stdout,
                    "completed",
                    Some("The live turn received Markdown."),
                )?;
                continue;
            }
            _ => bail!("unexpected method in the desktop provider fixture"),
        };
        writeln!(stdout, "{}", json!({"id":request["id"],"result":result}))?;
        stdout.flush()?;
    }
    Ok(())
}

fn composer_catalog(request: &Value) -> Value {
    let data = request["params"]["cwds"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter_map(|cwd| {
            let skills = composer_skills(Path::new(cwd));
            (!skills.is_empty()).then(|| json!({"cwd":cwd,"skills":skills}))
        })
        .collect::<Vec<_>>();
    json!({"data":data})
}

fn composer_skills(root: &Path) -> Vec<Value> {
    let installed = root
        .join("fixture-installed-plugins")
        .join("desktop-formatting");
    let mut skills = Vec::new();
    let local = root
        .join(".agents")
        .join("skills")
        .join("check-format")
        .join("SKILL.md");
    if local.is_file() {
        skills.push(json!({"name":"check-format","path":local,"scope":"repo","enabled":true}));
    }
    let export = installed
        .join("skills")
        .join("check-format")
        .join("SKILL.md");
    if installed.join(".codex-plugin/plugin.json").is_file() && export.is_file() {
        skills.push(
            json!({"name":"desktop-formatting:check-format","path":export,
            "scope":"user","enabled":true,"pluginId":COMPOSER_PLUGIN}),
        );
    }
    skills
}

fn verify_composer_input(root: &Path, request: &Value, text: &str) -> Result<()> {
    let input = request["params"]["input"]
        .as_array()
        .context("missing customization turn input")?;
    let skills = composer_skills(root);
    ensure!(
        skills.len() == 2,
        "missing the local and exported fixture skills"
    );
    let observed = input
        .iter()
        .filter(|item| item["type"] == "skill")
        .collect::<Vec<_>>();
    let expected = skills
        .into_iter()
        .map(|skill| json!({"type":"skill","name":skill["name"],"path":skill["path"]}))
        .collect::<Vec<_>>();
    ensure!(
        observed == expected.iter().collect::<Vec<_>>(),
        "the provider did not receive the two exact native skill inputs"
    );
    ensure!(
        text.contains("Quoted conversation references supplied by OmniAgent (JSON data):")
            && text.contains("Complete desktop slot probe")
            && text.contains("The freed slot ran the probe.")
            && !text.contains("@skill:"),
        "the provider did not receive the selected conversation excerpts separately from skill items"
    );
    Ok(())
}

fn question(
    output: &mut impl Write,
    id: i64,
    field: &str,
    label: &str,
    options: &[&str],
) -> Result<()> {
    let choices = options
        .iter()
        .map(|label| json!({"label":label}))
        .collect::<Vec<_>>();
    writeln!(
        output,
        "{}",
        json!({"id":id,"method":"item/tool/requestUserInput","params":{
            "threadId":SESSION,"turnId":TURN,"itemId":field,
            "questions":[{"id":field,"question":label,"options":choices}]
        }})
    )?;
    output.flush()?;
    Ok(())
}

fn finished(output: &mut impl Write, status: &str, answer: Option<&str>) -> Result<()> {
    if let Some(answer) = answer {
        writeln!(
            output,
            "{}",
            json!({"method":"item/completed","params":{
                "threadId":SESSION,"turnId":TURN,
                "item":{"type":"agentMessage","id":"answer","text":answer}
            }})
        )?;
    }
    writeln!(
        output,
        "{}",
        json!({"method":"turn/completed","params":{
            "threadId":SESSION,"turn":{"id":TURN,"status":status}
        }})
    )?;
    output.flush()?;
    Ok(())
}

fn wait_for_file(path: &Path) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(90);
    while !path.is_file() {
        ensure!(
            Instant::now() < deadline,
            "desktop test did not release the fixture"
        );
        thread::sleep(Duration::from_millis(30));
    }
    Ok(())
}

fn complete(root: &Path, prompt: &str) -> Result<&'static str> {
    if prompt.contains("Run the revised queued instruction") {
        ensure!(
            !prompt.contains("Replace this queued instruction")
                && !prompt.contains("Remove this queued instruction"),
            "obsolete instructions reached the provider"
        );
        fs::write(
            root.join("queued-instruction-dispatched"),
            "Run the revised queued instruction",
        )?;
        return Ok("The revised instruction ran.");
    }
    if prompt == "Complete desktop slot probe" {
        return Ok("The freed slot ran the probe.");
    }
    if let Some(slot) = prompt.strip_prefix("Hold desktop slot ") {
        let slot = slot.parse::<u32>()?;
        fs::write(root.join(format!("desktop-slot-{slot}")), [])?;
        wait_for_file(&root.join(RELEASE))?;
        return Ok("Released the held desktop job.");
    }
    ensure!(
        prompt == "Run the queued job while detached",
        "unexpected fixture prompt"
    );
    ensure!(
        root.join(RELEASE).is_file(),
        "the queued job started before capacity was released"
    );
    fs::write(root.join("detached-job-dispatched"), [])?;
    Ok("The queued job ran with the WebView detached.")
}
