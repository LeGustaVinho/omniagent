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
const MODEL: &str = "desktop-fixture";
const SESSION: &str = "desktop-fixture-session";
const TURN: &str = "desktop-fixture-turn";
const RELEASE: &str = "release-desktop-jobs";

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
            Some("skills/list") => json!({"data":[]}),
            Some("turn/start") => {
                writeln!(
                    stdout,
                    "{}",
                    json!({"id":request["id"],"result":{"turn":{"id":TURN}}})
                )?;
                stdout.flush()?;
                let answer = complete(&root, &request)?;
                writeln!(
                    stdout,
                    "{}",
                    json!({"method":"item/completed","params":{
                        "threadId":SESSION,"turnId":TURN,
                        "item":{"type":"agentMessage","id":"answer","text":answer}
                    }})
                )?;
                writeln!(
                    stdout,
                    "{}",
                    json!({"method":"turn/completed","params":{
                        "threadId":SESSION,"turn":{"id":TURN,"status":"completed"}
                    }})
                )?;
                stdout.flush()?;
                continue;
            }
            _ => bail!("unexpected method in the desktop provider fixture"),
        };
        writeln!(stdout, "{}", json!({"id":request["id"],"result":result}))?;
        stdout.flush()?;
    }
    Ok(())
}

fn complete(root: &Path, request: &Value) -> Result<&'static str> {
    let prompt = request
        .pointer("/params/input/0/text")
        .and_then(Value::as_str)
        .context("the fixture turn did not receive a text prompt")?;
    if let Some(slot) = prompt.strip_prefix("Hold desktop slot ") {
        let slot = slot.parse::<u32>()?;
        fs::write(root.join(format!("desktop-slot-{slot}")), [])?;
        let deadline = Instant::now() + Duration::from_secs(90);
        while !root.join(RELEASE).is_file() {
            ensure!(
                Instant::now() < deadline,
                "desktop test did not release the held job"
            );
            thread::sleep(Duration::from_millis(30));
        }
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
