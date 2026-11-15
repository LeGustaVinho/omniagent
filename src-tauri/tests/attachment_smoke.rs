use std::{
    ffi::{OsStr, OsString},
    fs::{self, File, OpenOptions},
    os::windows::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use omniagent::{
    attachments::{
        AttachmentFormat, AttachmentPreview, AttachmentSource, NativeAttachmentEncoding,
        TurnAttachment,
    },
    conversations::SessionScope,
    process_io::CapturedProcess,
    providers::{
        AdapterUpdate, ApprovalDecision, Provider, ProviderEventData, SessionIntent, SessionMode,
        SessionRequest, TurnInput, TurnOutcome,
        capabilities::{CapabilityStatus, CapabilitySubject, EvidenceOrigin, ObservedResult},
        profiles::{ConfiguredExecutable, probe_cli},
        runtime::start_managed_turn,
    },
    repository,
    runner::{CONTRACT_VERSION, DirectWindowsRunner, LaunchRequest, Runner},
    runs::RunStatus,
    store::Store,
};
use serde_json::{Value, json};
use uuid::Uuid;
use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ;

struct Media {
    format: AttachmentFormat,
    path: PathBuf,
    bytes: Vec<u8>,
    expected: Vec<String>,
}

fn image(directory: &Path) -> Result<Media> {
    const COLORS: [(&str, [u8; 3]); 4] = [
        ("red", [230, 24, 20]),
        ("green", [20, 190, 30]),
        ("blue", [25, 45, 230]),
        ("yellow", [242, 220, 27]),
    ];
    let selection = Uuid::new_v4();
    let tiles: Vec<_> = selection.as_bytes()[..6]
        .iter()
        .map(|byte| COLORS[*byte as usize % COLORS.len()])
        .collect();
    let mut pixels = Vec::new();
    let width = (tiles.len() * 82) as u32;
    let height = 92;
    for _ in 0..height {
        for (_, color) in &tiles {
            for x in 0..82 {
                pixels.extend(if !(3..79).contains(&x) {
                    &[245; 3]
                } else {
                    color
                });
            }
        }
    }
    let mut bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut bytes, width, height);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.write_header()?.write_image_data(&pixels)?;
    }
    let path = directory.join("cores da sessão.png");
    fs::write(&path, &bytes)?;
    Ok(Media {
        format: AttachmentFormat::Png,
        path,
        bytes,
        expected: tiles.into_iter().map(|(name, _)| name.to_owned()).collect(),
    })
}

fn document(directory: &Path) -> Result<Media> {
    let nonce = format!("OMNI{}", &Uuid::new_v4().simple().to_string()[..13]);
    let stream = format!("BT /F1 30 Tf 36 135 Td ({nonce}) Tj ET\n");
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".into(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 440 240] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>".into(),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into(),
        format!("<< /Length {} >>\nstream\n{}endstream", stream.len(), stream),
    ];
    let mut doc = String::from("%PDF-1.4\n");
    let mut offsets = Vec::new();
    for (index, object) in objects.iter().enumerate() {
        offsets.push(doc.len());
        doc.push_str(&format!("{} 0 obj\n{object}\nendobj\n", index + 1));
    }
    let xref = doc.len();
    doc.push_str("xref\n0 6\n0000000000 65535 f \n");
    for offset in offsets {
        doc.push_str(&format!("{offset:010} 00000 n \n"));
    }
    doc.push_str(&format!(
        "trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n"
    ));
    let bytes = doc.into_bytes();
    let path = directory.join("documento da sessão.pdf");
    fs::write(&path, &bytes)?;
    Ok(Media {
        format: AttachmentFormat::Pdf,
        path,
        bytes,
        expected: vec![nonce],
    })
}

fn embedded_text(directory: &Path) -> Result<Media> {
    let code = format!("TEXTO{}", &Uuid::new_v4().simple().to_string()[..13]);
    let bytes = format!("Código do recurso: {code}\n").into_bytes();
    let path = directory.join("recurso.txt");
    fs::write(&path, &bytes)?;
    Ok(Media {
        format: AttachmentFormat::Text,
        path,
        bytes,
        expected: vec![code],
    })
}

fn wave(directory: &Path) -> Result<Media> {
    let words = [
        "amber", "maple", "ocean", "lantern", "rose", "violet", "atlas", "river",
    ];
    let nonce = Uuid::new_v4();
    let expected: Vec<_> = nonce.as_bytes()[..6]
        .iter()
        .map(|byte| words[*byte as usize % words.len()].to_owned())
        .collect();
    let path = directory.join("voz.wav");
    let output = Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            r#"
Add-Type -AssemblyName System.Speech
$synthesizer = [System.Speech.Synthesis.SpeechSynthesizer]::new()
$synthesizer.SetOutputToWaveFile($env:OMNIAGENT_TEST_WAVE_PATH)
$synthesizer.Speak($env:OMNIAGENT_TEST_WAVE_WORDS)
$synthesizer.Dispose()
"#,
        ])
        .env("OMNIAGENT_TEST_WAVE_PATH", &path)
        .env("OMNIAGENT_TEST_WAVE_WORDS", expected.join(", "))
        .output()?;
    ensure!(
        output.status.success() && path.is_file(),
        "Windows could not synthesize the disposable audio fixture"
    );
    let bytes = fs::read(&path)?;
    Ok(Media {
        format: AttachmentFormat::Wav,
        path,
        bytes,
        expected,
    })
}

struct NativeObservation {
    sent: Vec<(AttachmentFormat, NativeAttachmentEncoding)>,
    unavailable: Vec<AttachmentFormat>,
    answer: String,
}

fn wire(process: &mut CapturedProcess, update: &AdapterUpdate) -> Result<()> {
    for frame in &update.outbound {
        process.input.send(frame.as_bytes())?;
    }
    Ok(())
}

fn verify_wire(provider: Provider, input: &[TurnAttachment], frame: &[u8]) -> Result<()> {
    let frame: Value = serde_json::from_slice(frame)?;
    let blocks = match provider {
        Provider::Codex => &frame["params"]["input"],
        Provider::Claude => &frame["message"]["content"],
        Provider::Antigravity => &frame["params"]["prompt"],
    }
    .as_array()
    .context("expected native content blocks")?;
    ensure!(blocks.len() == input.len() + 1 && blocks[0]["type"] == "text");
    for (block, attachment) in blocks[1..].iter().zip(input) {
        let expected = match attachment.encoding {
            NativeAttachmentEncoding::CodexLocalImage => {
                json!({"type":"localImage","path":attachment.path})
            }
            NativeAttachmentEncoding::ClaudeImage | NativeAttachmentEncoding::ClaudeDocument => {
                let kind = if attachment.format.is_image() {
                    "image"
                } else {
                    "document"
                };
                json!({"type":kind,"source":{"type":"base64","media_type":attachment.format.mime(),"data":attachment.content}})
            }
            NativeAttachmentEncoding::AcpImage | NativeAttachmentEncoding::AcpAudio => {
                let kind = if attachment.format.is_image() {
                    "image"
                } else {
                    "audio"
                };
                json!({"type":kind,"mimeType":attachment.format.mime(),"data":attachment.content})
            }
            NativeAttachmentEncoding::AcpResource => {
                let mut resource =
                    json!({"uri":attachment.resource_uri,"mimeType":attachment.format.mime()});
                if attachment.format == AttachmentFormat::Text {
                    resource["text"] =
                        json!(String::from_utf8(STANDARD.decode(&attachment.content)?)?);
                } else {
                    resource["blob"] = json!(attachment.content);
                }
                json!({"type":"resource","resource":resource})
            }
        };
        ensure!(
            block == &expected,
            "native attachment wire shape did not match the fixture"
        );
    }
    Ok(())
}

fn observe(
    provider: Provider,
    model: &str,
    checkout: &Path,
    media: &[Media],
) -> Result<NativeObservation> {
    let adapter = provider.adapter();
    let executable = adapter.discover(None)?;
    let mut session = adapter.create_session(SessionRequest {
        checkout: checkout.into(),
        intent: SessionIntent::Start,
        mode: SessionMode::ReadOnly,
        model: Some(model.into()),
        effort: None,
    })?;
    let mut arguments = session.arguments();
    if provider == Provider::Claude {
        arguments.retain(|arg| arg != "--safe-mode");
        arguments.extend(
            [
                "--restricted",
                "--system-prompt-snapshot",
                "off",
                "--settings",
                r#"{"hooks":{},"autoMemoryEnabled":false}"#,
            ]
            .map(OsString::from),
        );
    } else if provider == Provider::Codex {
        arguments.splice(
            ..0,
            ["-c", "mcp_servers={}", "-c", "project_doc_max_bytes=0"].map(OsString::from),
        );
    }
    let mut process = DirectWindowsRunner.launch(LaunchRequest {
        version: CONTRACT_VERSION,
        executable: &executable,
        checkout,
        arguments: &arguments
            .iter()
            .map(OsString::as_os_str)
            .collect::<Vec<&OsStr>>(),
    })?;
    wire(&mut process, &session.start()?)?;
    let mut reached_turn = false;
    let mut observation = NativeObservation {
        sent: Vec::new(),
        unavailable: Vec::new(),
        answer: String::new(),
    };
    let mut locks: Vec<File> = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(65);
    loop {
        let frame = process
            .output
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .context("attachment smoke timed out")??;
        let update = session.receive(frame)?;
        wire(&mut process, &update)?;
        for event in update.events {
            if let ProviderEventData::SessionReady {
                model: Some(reported),
                ..
            } = &event.data
            {
                ensure!(
                    reported == model,
                    "CLI selected a different model: {reported}"
                );
            }
            let ready = matches!(event.data, ProviderEventData::SessionReady { .. })
                || (provider == Provider::Claude
                    && matches!(event.data, ProviderEventData::Handshake { .. }));
            if ready && !reached_turn {
                let mut blocks = Vec::new();
                let mut tasks = Vec::new();
                for item in media {
                    let Some(encoding) = session.attachment_encoding(item.format) else {
                        observation.unavailable.push(item.format);
                        continue;
                    };
                    let path = if encoding == NativeAttachmentEncoding::CodexLocalImage {
                        Some(
                            DirectWindowsRunner
                                .attachment_path(&item.path)
                                .context("missing runner path mapping")?,
                        )
                    } else {
                        None
                    };
                    if let Some(path) = &path {
                        locks.push(
                            OpenOptions::new()
                                .read(true)
                                .share_mode(FILE_SHARE_READ)
                                .open(path)?,
                        );
                    }
                    blocks.push(TurnAttachment {
                        encoding,
                        format: item.format,
                        path,
                        content: STANDARD.encode(&item.bytes),
                        resource_uri: (encoding == NativeAttachmentEncoding::AcpResource)
                            .then(|| format!("urn:omniagent:fixture:{}", Uuid::new_v4())),
                    });
                    observation.sent.push((item.format, encoding));
                    tasks.push(match item.format {
                        AttachmentFormat::Png => {
                            "list the six image tile colors in order from left to right, repeating adjacent colors"
                        }
                        AttachmentFormat::Pdf => "return the exact code printed on the PDF page",
                        AttachmentFormat::Text => "return the exact code in the embedded plain-text resource",
                        AttachmentFormat::Wav => "transcribe the six spoken words in order",
                        _ => unreachable!(),
                    });
                }
                if blocks.is_empty() {
                    break;
                }
                let prompt = format!(
                    "Use only the supplied native content blocks. Do not read files or use tools. Please {}.",
                    tasks.join(" and ")
                );
                let update = session.send(TurnInput {
                    message_id: "attachment-fixture".into(),
                    text: prompt,
                    attachments: blocks.clone(),
                })?;
                verify_wire(
                    provider,
                    &blocks,
                    update
                        .outbound
                        .first()
                        .context("missing prompt frame")?
                        .as_bytes(),
                )?;
                wire(&mut process, &update)?;
                reached_turn = true;
            }
            match event.data {
                ProviderEventData::AssistantMessage { text } => observation.answer.push_str(&text),
                ProviderEventData::Approval { request_id, .. } => wire(
                    &mut process,
                    &session.respond_approval(&request_id, ApprovalDecision::Deny)?,
                )?,
                ProviderEventData::Error { message } => {
                    bail!("attachment rejected in {provider:?}: {message}")
                }
                ProviderEventData::TurnFinished { outcome } => {
                    ensure!(
                        outcome == TurnOutcome::Completed,
                        "attachment prompt did not complete"
                    );
                    process.control.cancel()?;
                    process.control.wait()?;
                    return Ok(observation);
                }
                _ => {}
            }
        }
        if !reached_turn && !observation.unavailable.is_empty() {
            break;
        }
    }
    process.control.cancel()?;
    process.control.wait()?;
    Ok(observation)
}

fn fixture() -> Result<(tempfile::TempDir, PathBuf)> {
    let owned = tempfile::tempdir()?;
    let checkout = dunce::canonicalize(owned.path())?;
    ensure!(
        Command::new("git.exe")
            .args(["init", "--quiet"])
            .current_dir(&checkout)
            .status()?
            .success()
    );
    Ok((owned, checkout))
}

fn verify_content(observation: &NativeObservation, media: &[Media]) -> Result<()> {
    for item in media.iter().filter(|item| {
        observation
            .sent
            .iter()
            .any(|(format, _)| *format == item.format)
    }) {
        ensure!(
            evidence_matches(item, &observation.answer),
            "attachment proof failed for {:?}, expected {:?}, observed {:?}",
            item.format,
            item.expected,
            observation.answer
        );
    }
    Ok(())
}

fn evidence_matches(item: &Media, answer: &str) -> bool {
    let mut words: Vec<_> = answer
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_ascii_lowercase)
        .collect();
    if item.format == AttachmentFormat::Png {
        words.retain(|word| matches!(word.as_str(), "red" | "green" | "blue" | "yellow"));
    }
    let expected: Vec<_> = item
        .expected
        .iter()
        .map(|word| word.to_ascii_lowercase())
        .collect();
    words
        .windows(expected.len())
        .any(|sequence| sequence == expected)
}

#[tokio::test]
#[ignore = "requires authenticated Codex with vision on gpt-6-luna"]
async fn codex_reads_a_scoped_local_image() -> Result<()> {
    let (_owned, checkout) = fixture()?;
    let stage = tempfile::tempdir()?;
    let media = [image(&dunce::canonicalize(stage.path())?)?];
    let observation = observe(Provider::Codex, "gpt-6-luna", &checkout, &media)?;
    assert_eq!(
        observation.sent,
        [(
            AttachmentFormat::Png,
            NativeAttachmentEncoding::CodexLocalImage
        )]
    );
    verify_content(&observation, &media)?;

    let store = Store::open(checkout.join("attachment-fixture.sqlite"))?;
    let checked_out = repository::register(&store, checkout.clone()).await?;
    let chat = store
        .create_conversation(checked_out.checkout_id, "Native image qualification".into())
        .await?;
    let probe = probe_cli(ConfiguredExecutable {
        provider: Provider::Codex,
        executable: None,
    })?;
    let scope = SessionScope {
        provider: Provider::Codex,
        profile_id: "checked-native-image-fixture".into(),
        account_binding: Some("authenticated-vendor-fixture".into()),
        checkout: checkout.clone(),
        executable: probe.executable.context("missing Codex executable")?.into(),
        cli_version: probe.version.context("missing Codex CLI build")?,
        runner: DirectWindowsRunner.identity().into(),
        permission_revision: "read-only-native-fixture".into(),
        mode: SessionMode::ReadOnly,
        model: Some("gpt-6-luna".into()),
        effort: None,
    };
    let baseline = store
        .prepare_agent_attempt(
            chat.id.clone(),
            chat.context_id,
            scope.clone(),
            "Reply READY.".into(),
        )
        .await?;
    let baseline_id = baseline.run.id.clone();
    let runner = Arc::new(DirectWindowsRunner);
    assert_eq!(
        tokio::time::timeout(
            Duration::from_secs(65),
            start_managed_turn(store.clone(), runner.clone(), baseline, None).wait()
        )
        .await??,
        RunStatus::Succeeded
    );
    let loaded = store
        .agent_dispatch(chat.id.clone(), baseline_id)
        .await?
        .context("missing native input profile snapshot")?;
    let assessment = loaded
        .snapshot
        .capabilities
        .context("missing native input assessment")?;
    let subject = CapabilitySubject::NativeAttachment {
        format: AttachmentFormat::Png,
        encoding: NativeAttachmentEncoding::CodexLocalImage,
    };
    let chosen = assessment
        .entries
        .iter()
        .find(|entry| entry.descriptor.subject == subject)
        .context("missing local image route")?;
    let initial_status = chosen.status();
    ensure!(
        matches!(
            initial_status,
            CapabilityStatus::Unverified | CapabilityStatus::Reported
        ),
        "unexpected native image availability: {initial_status:?}"
    );
    let evidence_id = store
        .record_capability_evidence(
            assessment.scope,
            chosen.descriptor.clone(),
            EvidenceOrigin::AuthenticatedFixture,
            ObservedResult::Succeeded,
            format!("{}:native-local-image", Uuid::new_v4()),
        )
        .await?;

    let chat = store.conversation(chat.id).await?;
    let preview = AttachmentPreview::read(
        &checkout,
        AttachmentSource::SelectedFile {
            path: media[0].path.clone(),
        },
    )?;
    let attached = store.prepare_agent_attempt_with_attachments(chat.id.clone(), chat.context_id, scope,
        "Use only the supplied image. List all six colors left to right and repeat colors when needed. Do not use tools.".into(), vec![preview]).await?;
    let run = attached.run.id.clone();
    assert_eq!(
        tokio::time::timeout(
            Duration::from_secs(65),
            start_managed_turn(store.clone(), runner, attached, None).wait()
        )
        .await??,
        RunStatus::Succeeded
    );
    let saved = store
        .agent_dispatch(chat.id.clone(), run)
        .await?
        .context("missing image wire evidence")?;
    assert_eq!(saved.snapshot.attachments[0].qualification_id, evidence_id);
    assert_eq!(
        STANDARD.decode(&saved.snapshot.attachments[0].input.content)?,
        media[0].bytes
    );
    let messages = store.conversation_messages(chat.id).await?;
    assert!(evidence_matches(
        &media[0],
        &messages
            .last()
            .context("missing managed image result")?
            .text
    ));
    Ok(())
}

#[test]
#[ignore = "requires authenticated Claude Code with Haiku 5.5"]
fn claude_reads_image_and_pdf_inline_in_stream_json() -> Result<()> {
    let (_owned, checkout) = fixture()?;
    let media = [image(&checkout)?, document(&checkout)?];
    let observation = observe(Provider::Claude, "claude-haiku-5-5", &checkout, &media)?;
    assert_eq!(
        observation.sent,
        [
            (AttachmentFormat::Png, NativeAttachmentEncoding::ClaudeImage),
            (
                AttachmentFormat::Pdf,
                NativeAttachmentEncoding::ClaudeDocument
            )
        ]
    );
    verify_content(&observation, &media)
}

#[test]
#[ignore = "requires authenticated Antigravity with Gemini 3.8 Flash"]
fn antigravity_checks_its_advertised_native_inputs() -> Result<()> {
    let (_owned, checkout) = fixture()?;
    for item in [
        image(&checkout)?,
        document(&checkout)?,
        embedded_text(&checkout)?,
        wave(&checkout)?,
    ] {
        let observation = observe(
            Provider::Antigravity,
            "gemini-3.8-flash",
            &checkout,
            std::slice::from_ref(&item),
        )?;
        assert_eq!(observation.sent.len() + observation.unavailable.len(), 1);
        if let Some((format, encoding)) = observation.sent.first() {
            let consumed = evidence_matches(&item, &observation.answer);
            eprintln!(
                "ACP {:?} via {:?}: {}",
                format,
                encoding,
                if consumed {
                    "verified input"
                } else {
                    "unverified input"
                }
            );
            if *format != AttachmentFormat::Pdf {
                verify_content(&observation, std::slice::from_ref(&item))?;
            }
        } else {
            assert_eq!(observation.unavailable, [item.format]);
            eprintln!("ACP {:?}: format capability was unavailable", item.format);
        }
    }
    Ok(())
}
