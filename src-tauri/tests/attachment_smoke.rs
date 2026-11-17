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
        AttachmentDelivery, AttachmentFormat, AttachmentInput, AttachmentPreview, AttachmentSource,
        NativeAttachmentEncoding, QualifiedAttachmentLimits, TurnAttachment,
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
                    attachments: blocks
                        .iter()
                        .cloned()
                        .map(AttachmentInput::Native)
                        .collect(),
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
    let attached = store.prepare_agent_attempt_with_attachments(chat.id.clone(), chat.context_id, scope.clone(),
        "Use only the supplied image. List all six colors left to right and repeat colors when needed. Do not use tools.".into(), vec![preview]).await?;
    let run = attached.run.id.clone();
    assert_eq!(
        tokio::time::timeout(
            Duration::from_secs(65),
            start_managed_turn(store.clone(), runner.clone(), attached, None).wait()
        )
        .await??,
        RunStatus::Succeeded
    );
    let saved = store
        .agent_dispatch(chat.id.clone(), run)
        .await?
        .context("missing image wire evidence")?;
    assert_eq!(
        saved.snapshot.attachments[0].qualification_id.as_deref(),
        Some(evidence_id.as_str())
    );
    assert_eq!(
        STANDARD.decode(
            &saved.snapshot.attachments[0]
                .input
                .native()
                .unwrap()
                .content
        )?,
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

    let candidate = store
        .create_conversation(chat.checkout_id, "Different profile".into())
        .await?;
    let mut switched = scope;
    switched.profile_id = "unqualified-native-image-fixture".into();
    let preview = store
        .preview_attachment(
            candidate.checkout_id.clone(),
            AttachmentSource::SelectedFile {
                path: media[0].path.clone(),
            },
            AttachmentDelivery::Native,
        )
        .await?;
    let plan = store
        .prepare_agent_attempt_with_attachments(
            candidate.id.clone(),
            candidate.context_id,
            switched,
            "Inspect the same image.".into(),
            vec![preview],
        )
        .await?;
    let run = plan.run.id.clone();
    assert_eq!(
        start_managed_turn(store.clone(), runner, plan, None)
            .wait()
            .await?,
        RunStatus::Failed
    );
    assert!(
        store
            .agent_dispatch(candidate.id.clone(), run.clone())
            .await?
            .is_none()
    );
    let events = store.agent_observations(candidate.id, run, 0).await?;
    assert!(events.iter().any(|entry| match &entry.event.data {
        ProviderEventData::AttachmentBlocked { blockers } =>
            blockers.len() == 1
                && blockers[0].delivery == AttachmentDelivery::Native
                && blockers[0].reason.contains("unverified for this profile"),
        _ => false,
    }));
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

#[tokio::test]
#[ignore = "requires authenticated Antigravity with Gemini 3.8 Flash"]
async fn antigravity_reads_a_resource_link_and_receives_the_previewed_excerpt() -> Result<()> {
    verify_fallback_routes(Provider::Antigravity, "gemini-3.8-flash").await
}

#[tokio::test]
#[ignore = "requires authenticated Codex with gpt-6-luna"]
async fn codex_reads_a_local_reference_and_receives_the_previewed_excerpt() -> Result<()> {
    verify_fallback_routes(Provider::Codex, "gpt-6-luna").await
}

#[tokio::test]
#[ignore = "requires authenticated Claude Code with Haiku 5.5"]
async fn claude_reads_a_local_reference_and_receives_the_previewed_excerpt() -> Result<()> {
    verify_fallback_routes(Provider::Claude, "claude-haiku-5-5").await
}

async fn verify_fallback_routes(provider: Provider, model: &str) -> Result<()> {
    let (_owned, checkout) = fixture()?;
    let external = tempfile::tempdir()?;
    let store = Store::open(checkout.join("fallback-fixture.sqlite"))?;
    let repo = repository::register(&store, checkout.clone()).await?;
    let chat = store
        .create_conversation(repo.checkout_id, "Attachment fallback fixture".into())
        .await?;
    let oversized_path = "oversized.bin";
    File::create(checkout.join(oversized_path))?.set_len(20 * 1024 * 1024 + 1)?;
    let oversized = store
        .preview_attachment(
            chat.checkout_id.clone(),
            AttachmentSource::Checkout {
                relative_path: oversized_path.into(),
            },
            AttachmentDelivery::Native,
        )
        .await
        .err()
        .context("oversized native attachment was previewed as eligible")?;
    assert!(
        oversized.to_string().contains("oversized.bin")
            && oversized.to_string().contains("20971520 bytes")
    );
    let probe = probe_cli(ConfiguredExecutable {
        provider,
        executable: None,
    })?;
    let scope = SessionScope {
        provider,
        profile_id: "fallback-fixture".into(),
        account_binding: Some("authenticated-vendor-fixture".into()),
        checkout: checkout.clone(),
        executable: probe.executable.context("missing executable")?.into(),
        cli_version: probe.version.context("missing CLI build")?,
        runner: DirectWindowsRunner.identity().into(),
        permission_revision: "read-only-fallback-fixture".into(),
        mode: SessionMode::ReadOnly,
        model: Some(model.into()),
        effort: None,
    };
    let link_code = Uuid::new_v4().simple().to_string();
    let excerpt_code = Uuid::new_v4().simple().to_string();
    let relative_path = PathBuf::from("link #fixture.txt");
    let reference = if provider == Provider::Antigravity {
        checkout.join(&relative_path)
    } else {
        dunce::canonicalize(external.path())?.join(&relative_path)
    };
    fs::write(&reference, &link_code)?;
    fs::write(
        checkout.join("excerpt.txt"),
        format!("{excerpt_code}\nunselected"),
    )?;
    let selected_reference = AttachmentPreview::read_with_delivery(
        &checkout,
        if provider == Provider::Antigravity {
            AttachmentSource::Checkout { relative_path }
        } else {
            AttachmentSource::SelectedFile {
                path: reference.clone(),
            }
        },
        AttachmentDelivery::Reference,
    )?;
    let excerpt = AttachmentPreview::read_with_delivery(
        &checkout,
        AttachmentSource::Checkout {
            relative_path: "excerpt.txt".into(),
        },
        AttachmentDelivery::Excerpt {
            start: 0,
            end: excerpt_code.len() as u64,
        },
    )?;
    let plan = store.prepare_agent_attempt_with_attachments(
        chat.id.clone(), chat.context_id, scope.clone(),
        "Reply with the two exact codes. If the attached file reference needs opening, use the built-in file reader. The other code is in the attached excerpt.".into(),
        vec![excerpt, selected_reference],
    ).await?;
    let run = plan.run.id.clone();
    let result = tokio::time::timeout(
        Duration::from_secs(65),
        start_managed_turn(store.clone(), Arc::new(DirectWindowsRunner), plan, None).wait(),
    )
    .await
    .context("fallback attachment prompt did not settle")??;
    let saved = store
        .agent_dispatch(chat.id.clone(), run.clone())
        .await?
        .context("missing fallback dispatch")?;
    let frame: Value = serde_json::from_slice(&saved.frames[0])?;
    let blocks = match provider {
        Provider::Antigravity => &frame["params"]["prompt"],
        Provider::Codex => &frame["params"]["input"],
        Provider::Claude => &frame["message"]["content"],
    };
    assert_eq!(
        blocks[1]["text"],
        format!(
            "Excerpt from attachment \"excerpt.txt\" (UTF-8 bytes 0..{}):\n{excerpt_code}",
            excerpt_code.len()
        )
    );
    let dispatched_path = saved.snapshot.attachments[1].input.path().unwrap();
    if provider == Provider::Antigravity {
        assert_eq!(dispatched_path, reference);
        assert_eq!(
            blocks[2],
            json!({
                "type": "resource_link", "uri": tauri::Url::from_file_path(&reference).unwrap().as_str(),
                "name": "link #fixture.txt", "mimeType": "text/plain", "size": link_code.len(),
            })
        );
    } else {
        assert_ne!(dispatched_path, reference);
        assert!(!dispatched_path.exists());
        assert_eq!(fs::read_to_string(reference)?, link_code);
        assert_eq!(
            blocks[2],
            json!({
                "type":"text",
                "text":format!("Attachment reference \"link #fixture.txt\" (local file path): {}", dispatched_path.display()),
            })
        );
    }
    ensure!(
        result == RunStatus::Succeeded,
        "{provider:?} reference prompt failed: {:?}",
        store
            .agent_observations(chat.id.clone(), run, 0)
            .await?
            .into_iter()
            .map(|entry| entry.event.data)
            .collect::<Vec<_>>()
    );
    let messages = store.conversation_messages(chat.id).await?;
    let answer = &messages.last().context("missing fallback answer")?.text;
    ensure!(
        answer.contains(&excerpt_code) && answer.contains(&link_code),
        "{provider:?} did not consume both routes: {answer}"
    );

    let encoded_bytes = saved
        .frames
        .iter()
        .map(|frame| frame.len() as u64)
        .sum::<u64>();
    let capability_scope = saved
        .snapshot
        .capabilities
        .context("missing capability assessment")?
        .scope;
    let prior_plugin_status = saved
        .snapshot
        .plugin_inventory
        .as_ref()
        .map(|inventory| inventory.report.status);
    // Qualify the envelope actually exercised by the authenticated fixture.
    let evidence = store
        .record_attachment_limit_evidence(
            capability_scope.clone(),
            saved.id,
            QualifiedAttachmentLimits {
                files_per_turn: Some(2),
                encoded_request_bytes: Some(encoded_bytes),
                ..Default::default()
            },
        )
        .await?;
    let checked = store
        .create_conversation(
            chat.checkout_id.clone(),
            "Attachment request limit fixture".into(),
        )
        .await?;
    let preview = store
        .preview_attachment(
            chat.checkout_id,
            AttachmentSource::Checkout {
                relative_path: "excerpt.txt".into(),
            },
            AttachmentDelivery::Excerpt { start: 0, end: 1 },
        )
        .await?;
    let blocked = store
        .prepare_agent_attempt_with_attachments(
            checked.id.clone(),
            checked.context_id,
            scope,
            format!(
                "Use the attached excerpt {}",
                "x".repeat(encoded_bytes as usize)
            ),
            vec![preview],
        )
        .await?;
    let blocked_run = blocked.run.id.clone();
    let run_status = tokio::time::timeout(
        Duration::from_secs(35),
        start_managed_turn(store.clone(), Arc::new(DirectWindowsRunner), blocked, None).wait(),
    )
    .await
    .context("qualified request limit did not settle")??;
    if let Some(unexpected) = store
        .agent_dispatch(checked.id.clone(), blocked_run.clone())
        .await?
    {
        let plugin_status = unexpected
            .snapshot
            .plugin_inventory
            .as_ref()
            .map(|inventory| inventory.report.status);
        let observed = unexpected
            .snapshot
            .capabilities
            .context("missing unexpected capability assessment")?
            .scope;
        let changed_sources: Vec<_> = capability_scope
            .configuration
            .detected
            .iter()
            .chain(&observed.configuration.detected)
            .filter(|entry| {
                !capability_scope.configuration.detected.contains(entry)
                    || !observed.configuration.detected.contains(entry)
            })
            .filter_map(|entry| entry.path.file_name())
            .collect();
        let previous = serde_json::to_value(&capability_scope)?;
        let current = serde_json::to_value(&observed)?;
        let differing_fields = |before: &Value, after: &Value| -> Result<Vec<String>> {
            let before = before
                .as_object()
                .context("Expected previous capability fields")?;
            let after = after
                .as_object()
                .context("Expected current capability fields")?;
            Ok(before
                .keys()
                .chain(after.keys())
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .filter(|key| before.get(*key) != after.get(*key))
                .cloned()
                .collect())
        };
        let scope_changes = differing_fields(&previous, &current)?;
        let config_changes =
            differing_fields(&previous["configuration"], &current["configuration"])?;
        bail!(
            "{provider:?} did not apply the qualified request envelope ({run_status:?}). Scope changes: {scope_changes:?}, observed model: {:?} -> {:?}, configuration changes: {config_changes:?}, changed sources: {changed_sources:?}, matching unresolved paths: {}, plugin inventory: {prior_plugin_status:?} -> {plugin_status:?}",
            capability_scope.reported_model,
            observed.reported_model,
            capability_scope.configuration.unresolved == observed.configuration.unresolved,
        );
    }
    assert_eq!(run_status, RunStatus::Failed);
    assert!(
        store
            .agent_dispatch(checked.id.clone(), blocked_run.clone())
            .await?
            .is_none()
    );
    let report = store
        .agent_observations(checked.id, blocked_run, 0)
        .await?
        .into_iter()
        .find_map(|entry| match entry.event.data {
            ProviderEventData::AttachmentAssessed { assessment } => Some(assessment),
            _ => None,
        })
        .context("missing request limit decisions")?;
    assert_eq!(report.qualification_id.as_deref(), Some(evidence.as_str()));
    assert!(report.encoded_request_bytes.unwrap() > encoded_bytes);
    assert!(
        report.blockers[0]
            .reason
            .contains(&format!("qualified limit is {encoded_bytes} bytes"))
    );
    Ok(())
}
