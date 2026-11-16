use std::{
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
    sync::mpsc::{RecvTimeoutError, sync_channel},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::{
    process::TerminalOutcome,
    process_io::Stream,
    runner::{CONTRACT_VERSION, DirectWindowsRunner, LaunchRequest, Runner},
};

const MAX_AUDIO_BYTES: usize = 256 * 1024;
const TOOL: &str = "omniagent_validate_output";
const PHRASE: &str = "Verification complete.";
const CAPTION_BEFORE: &str = "OmniAgent disposable fixture pending";
const CAPTION_AFTER: &str = "OmniAgent disposable fixture activated";

#[derive(Debug, Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputFixture {
    AudioGeneration,
    BrowserControl,
    HostControl,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputFixtureReceipt {
    pub tool: String,
    pub revision: u16,
    pub provider_thread_id: String,
    pub provider_turn_id: String,
    pub provider_call_id: String,
    pub result: OutputFixtureResult,
    pub checked_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum OutputFixtureResult {
    AudioGeneration {
        voice: String,
        phrase: String,
        byte_len: usize,
        sha256: String,
    },
    BrowserControl {
        browser: String,
        target: String,
        operation: String,
        result: String,
    },
    HostControl {
        operation: String,
        before: String,
        after: String,
    },
}

pub(super) struct ExecutedFixture {
    pub result: OutputFixtureResult,
    pub audio: Option<Vec<u8>>,
}

impl OutputFixture {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::AudioGeneration => "Validate managed audio generation",
            Self::BrowserControl => "Validate managed browser control",
            Self::HostControl => "Validate managed Windows host control",
        }
    }

    pub(crate) fn available(self) -> bool {
        match self {
            Self::AudioGeneration | Self::HostControl => powershell().is_some(),
            Self::BrowserControl => browser().is_some(),
        }
    }

    pub(super) fn definition(self) -> Value {
        json!({
            "type": "function",
            "name": TOOL,
            "description": self.label(),
            "inputSchema": {"type":"object","properties":{},"additionalProperties":false},
        })
    }

    pub(super) fn prompt(self) -> String {
        format!(
            "For this approved fixture ({}) call {TOOL} exactly once with the empty object. The fixture target is already selected by the host. Reply only with the tool status.",
            self.label()
        )
    }

    pub(super) fn execute(
        self,
        scratch: &Path,
        stopped: &dyn Fn() -> bool,
    ) -> Result<ExecutedFixture> {
        match self {
            Self::AudioGeneration => audio(scratch, stopped),
            Self::BrowserControl => browser_action(scratch, stopped),
            Self::HostControl => host_action(scratch, stopped),
        }
    }
}

pub(super) fn accepts_call(params: &Value, thread: &str, turn: &str) -> bool {
    params["tool"] == TOOL
        && params["threadId"] == thread
        && params["turnId"] == turn
        && params["namespace"].is_null()
        && params["arguments"]
            .as_object()
            .is_some_and(|args| args.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn managed_output_fixture_requires_an_exact_call_without_model_supplied_targets() {
        let mut params =
            json!({"tool": TOOL, "threadId": "thread", "turnId": "turn", "arguments": {}});
        assert!(accepts_call(&params, "thread", "turn"));
        assert!(!accepts_call(&params, "other", "turn"));
        assert!(!accepts_call(&params, "thread", "other"));
        params["arguments"]["target"] = "https://outside-fixture.test".into();
        assert!(!accepts_call(&params, "thread", "turn"));
    }
}

pub(super) fn accepted_reply(id: &Value) -> Value {
    json!({"id":id,"result":{"success":true,"contentItems":[{"type":"inputText","text":"The approved fixture completed and the host verified its result."}]}})
}

fn powershell() -> Option<PathBuf> {
    let executable = PathBuf::from(std::env::var_os("SystemRoot")?)
        .join("System32/WindowsPowerShell/v1.0/powershell.exe");
    executable.is_file().then_some(executable)
}

fn browser() -> Option<(PathBuf, &'static str)> {
    ["ProgramFiles(x86)", "ProgramFiles"]
        .into_iter()
        .filter_map(std::env::var_os)
        .map(PathBuf::from)
        .flat_map(|root| {
            [
                (
                    root.join("Microsoft/Edge/Application/msedge.exe"),
                    "Microsoft Edge",
                ),
                (
                    root.join("Google/Chrome/Application/chrome.exe"),
                    "Google Chrome",
                ),
            ]
        })
        .find(|(executable, _)| executable.is_file())
}

fn run_output(
    executable: &Path,
    scratch: &Path,
    arguments: &[&OsStr],
    output_limit: usize,
    stopped: &dyn Fn() -> bool,
) -> Result<String> {
    ensure!(!stopped(), "Fixture cancelled before launch");
    let process = DirectWindowsRunner.launch(LaunchRequest {
        version: CONTRACT_VERSION,
        executable,
        checkout: scratch,
        arguments,
    })?;
    let control = process.control.clone();
    let (completed, completion) = sync_channel(1);
    if let Err(error) = std::thread::Builder::new()
        .name("output-fixture-wait".into())
        .spawn(move || {
            let _ = completed.send(control.wait());
        })
    {
        super::stop_process(&process)?;
        return Err(error.into());
    }
    let result = (|| {
        let mut output = String::new();
        let mut stderr = String::new();
        let deadline = Instant::now() + Duration::from_secs(12);
        loop {
            ensure!(
                !stopped() && Instant::now() < deadline,
                "Fixture did not finish"
            );
            match process.output.recv_timeout(Duration::from_millis(100)) {
                Ok(frame) => {
                    let frame = frame?;
                    if frame.stream == Stream::Stdout {
                        ensure!(
                            output.len() + frame.data.len() <= output_limit,
                            "Fixture output exceeds its limit"
                        );
                        output.push_str(&frame.data);
                    } else if stderr.is_empty() {
                        stderr = crate::diagnostics::preview(&frame.data);
                    }
                }
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
        loop {
            ensure!(
                !stopped() && Instant::now() < deadline,
                "Fixture process did not exit"
            );
            match completion.recv_timeout(Duration::from_millis(100)) {
                Ok(outcome) => {
                    let outcome = outcome?;
                    ensure!(
                        outcome == TerminalOutcome::Exited(0),
                        "Fixture failed ({outcome:?}) {stderr}"
                    );
                    break;
                }
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => {
                    anyhow::bail!("Fixture waiter stopped unexpectedly")
                }
            }
        }
        process
            .control
            .confirm_quiescence()
            .context(super::SideProcessStillActive)?;
        Ok(output)
    })();
    if result.is_err() {
        super::stop_process(&process)?;
    }
    result
}

fn read_powershell<T: serde::de::DeserializeOwned>(
    source: &str,
    scratch: &Path,
    output_limit: usize,
    stopped: &dyn Fn() -> bool,
) -> Result<T> {
    let shell = powershell().context("Windows PowerShell is unavailable")?;
    let script = format!(
        r#"
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
{source}
"#
    );
    let encoded = STANDARD.encode(
        script
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>(),
    );
    let args = [
        "-NoLogo",
        "-NoProfile",
        "-NonInteractive",
        "-EncodedCommand",
        &encoded,
    ]
    .map(OsStr::new);
    Ok(serde_json::from_str(&run_output(
        &shell,
        scratch,
        &args,
        output_limit,
        stopped,
    )?)?)
}

fn audio(scratch: &Path, stopped: &dyn Fn() -> bool) -> Result<ExecutedFixture> {
    #[derive(Deserialize)]
    struct Synthesis {
        voice: String,
        data: String,
    }
    let source = format!(
        r#"
Add-Type -AssemblyName System.Speech
$synth = [System.Speech.Synthesis.SpeechSynthesizer]::new()
try {{
    $stream = [System.IO.MemoryStream]::new()
    $synth.SetOutputToWaveStream($stream)
    $synth.Speak('{PHRASE}')
    $synth.SetOutputToNull()
    [pscustomobject]@{{voice=$synth.Voice.Name;data=[Convert]::ToBase64String($stream.ToArray())}} | ConvertTo-Json -Compress
}} finally {{ $synth.Dispose() }}
"#
    );
    let generated: Synthesis = read_powershell(
        &source,
        scratch,
        MAX_AUDIO_BYTES.div_ceil(3) * 4 + 2048,
        stopped,
    )?;
    let wave = STANDARD.decode(generated.data)?;
    ensure!(
        !generated.voice.is_empty() && wave.len() <= MAX_AUDIO_BYTES && usable_wave(&wave),
        "Speech synthesis returned no playable PCM audio"
    );
    Ok(ExecutedFixture {
        result: OutputFixtureResult::AudioGeneration {
            voice: generated.voice,
            phrase: PHRASE.into(),
            byte_len: wave.len(),
            sha256: format!("{:x}", Sha256::digest(&wave)),
        },
        audio: Some(wave),
    })
}

fn usable_wave(wave: &[u8]) -> bool {
    fn inspect(wave: &[u8]) -> Option<bool> {
        let length = u32::from_le_bytes(wave.get(4..8)?.try_into().ok()?) as usize;
        if wave.get(..4)? != b"RIFF"
            || wave.get(8..12)? != b"WAVE"
            || length != wave.len().checked_sub(8)?
        {
            return Some(false);
        }
        let mut chunks = wave.get(12..)?;
        let mut format = false;
        let mut audio = false;
        while chunks.len() >= 8 {
            let (header, remaining) = chunks.split_at(8);
            let size = u32::from_le_bytes(header[4..8].try_into().ok()?) as usize;
            let block = remaining.get(..size)?;
            match &header[..4] {
                b"fmt " => {
                    let [format_lo, format_hi, channel_lo, channel_hi, ..] = block else {
                        return None;
                    };
                    let rate = u32::from_le_bytes(block.get(4..8)?.try_into().ok()?);
                    format = [*format_lo, *format_hi] == [1, 0]
                        && [*channel_lo, *channel_hi] == [1, 0]
                        && (8_000..=48_000).contains(&rate)
                        && block.get(14..16)? == [16, 0];
                }
                b"data" => {
                    audio = size >= 3200 && size % 2 == 0 && block.iter().any(|sample| *sample != 0)
                }
                _ => {}
            }
            chunks = remaining.get(size + size % 2..)?;
        }
        Some(format && audio && chunks.is_empty())
    }
    inspect(wave).unwrap_or(false)
}

fn browser_action(scratch: &Path, stopped: &dyn Fn() -> bool) -> Result<ExecutedFixture> {
    let (executable, browser) = browser().context("A supported browser is unavailable")?;
    let profile = scratch.join("profile");
    let page = scratch.join("fixture.html");
    let nonce = uuid::Uuid::new_v4();
    let expected = format!("activated-{nonce}");
    // Only this local button has an event handler. No repository data, network URL,
    // user browser profile, or script from the agent enters the browser process.
    std::fs::write(
        &page,
        format!(
            r#"<!doctype html>
<meta charset="utf-8"><meta http-equiv="Content-Security-Policy" content="default-src 'none'; script-src 'unsafe-inline'">
<button id="fixture-action">Run</button><output id="result">pending</output>
<script>
const button = document.getElementById('fixture-action');
button.addEventListener('click', () => {{ document.getElementById('result').textContent = '{expected}'; }});
button.click();
</script>"#
        ),
    )?;
    let url = tauri::Url::from_file_path(&page)
        .map_err(|_| anyhow::anyhow!("Cannot address the disposable test page"))?;
    let mut profile_argument = OsString::from("--user-data-dir=");
    profile_argument.push(profile);
    let mut args = [
        "--headless=new",
        "--disable-gpu",
        "--no-first-run",
        "--no-default-browser-check",
        "--disable-extensions",
        "--disable-default-apps",
        "--disable-background-networking",
        "--disable-component-update",
        "--disable-sync",
        "--no-proxy-server",
        "--host-resolver-rules=MAP * ~NOTFOUND",
        "--dump-dom",
    ]
    .into_iter()
    .map(OsStr::new)
    .collect::<Vec<_>>();
    args.extend([profile_argument.as_os_str(), OsStr::new(url.as_str())]);
    let dom = run_output(&executable, scratch, &args, 32 * 1024, stopped)?;
    ensure!(
        dom.contains(&format!("<output id=\"result\">{expected}</output>")),
        "The browser did not activate the fixture button"
    );
    Ok(ExecutedFixture {
        result: OutputFixtureResult::BrowserControl {
            browser: browser.into(),
            target: "Disposable local test page · button#fixture-action".into(),
            operation: "Invoke button click in the browser DOM".into(),
            result: "The page changed the test output from pending to activated".into(),
        },
        audio: None,
    })
}

fn host_action(scratch: &Path, stopped: &dyn Fn() -> bool) -> Result<ExecutedFixture> {
    #[derive(Deserialize)]
    struct Captions {
        before: String,
        after: String,
    }
    let source = format!(
        r#"
Add-Type -AssemblyName System.Windows.Forms
Add-Type -TypeDefinition @'
using System;
using System.Text;
using System.Runtime.InteropServices;
public class FixtureWin32 {{
    [DllImport("user32.dll", CharSet=CharSet.Unicode)]
    public static extern int GetWindowText(IntPtr hwnd, StringBuilder text, int capacity);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)]
    public static extern bool SetWindowText(IntPtr hwnd, string text);
}}
'@
$form = [System.Windows.Forms.Form]::new()
try {{
    $form.Text = '{CAPTION_BEFORE}'
    $handle = $form.Handle
    $caption = [System.Text.StringBuilder]::new(128)
    if ([FixtureWin32]::GetWindowText($handle, $caption, $caption.Capacity) -le 0) {{ throw 'Starting caption unavailable' }}
    $before = $caption.ToString()
    if (-not [FixtureWin32]::SetWindowText($handle, '{CAPTION_AFTER}')) {{ throw 'SetWindowText failed' }}
    if ([FixtureWin32]::GetWindowText($handle, $caption, $caption.Capacity) -le 0) {{ throw 'Result caption unavailable' }}
    [pscustomobject]@{{before=$before;after=$caption.ToString()}} | ConvertTo-Json -Compress
}} finally {{ $form.Dispose() }}
"#
    );
    let captions: Captions = read_powershell(&source, scratch, 2048, stopped)?;
    ensure!(
        captions.before == CAPTION_BEFORE && captions.after == CAPTION_AFTER,
        "The disposable Windows window did not report the expected change"
    );
    Ok(ExecutedFixture {
        result: OutputFixtureResult::HostControl {
            operation: "SetWindowText on the fixture's own Win32 window".into(),
            before: captions.before,
            after: captions.after,
        },
        audio: None,
    })
}
