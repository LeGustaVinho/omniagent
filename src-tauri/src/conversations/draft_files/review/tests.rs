use base64::{Engine, engine::general_purpose::STANDARD};

use crate::attachments::{QualifiedAttachmentContext, QualifiedAttachmentFormat};
use crate::conversations::tests::setup;
use crate::providers::Provider;

use super::*;

#[tokio::test]
async fn native_defaults_are_qualified_and_budget_blockers_leave_reviewable_fallbacks() -> Result<()>
{
    let (_checkout, _store, _chat, mut scope) = setup().await?;
    scope.provider = Provider::Claude;
    let png = STANDARD.decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aUzUAAAAASUVORK5CYII=")?;
    std::fs::write(scope.checkout.join("image.png"), &png)?;
    std::fs::write(scope.checkout.join("notes.txt"), "café\nsecond line")?;
    let image = DraftAttachment {
        id: "image".into(),
        source: AttachmentSource::Checkout {
            relative_path: "image.png".into(),
        },
        delivery: None,
    };
    let notes = DraftAttachment {
        id: "notes".into(),
        source: AttachmentSource::Checkout {
            relative_path: "notes.txt".into(),
        },
        delivery: None,
    };
    let qualifies = |file_bytes, encoded, count, window| Qualification {
        native: vec![(
            AttachmentFormat::Png,
            NativeAttachmentEncoding::ClaudeImage,
            "verified-profile".into(),
        )],
        limits: Some(QualifiedAttachmentLimits {
            files_per_turn: Some(count),
            encoded_request_bytes: Some(encoded),
            formats: vec![QualifiedAttachmentFormat {
                format: AttachmentFormat::Png,
                encoding: NativeAttachmentEncoding::ClaudeImage,
                file_bytes: Some(file_bytes),
                input_tokens: Some(90),
            }],
            context: Some(QualifiedAttachmentContext {
                window_tokens: window,
                reserved_output_tokens: 60,
                fixed_input_tokens: 20,
            }),
        }),
    };
    let project = AttachmentLimits {
        utf8_bytes_per_excerpt: 4,
        ..AttachmentLimits::default()
    };
    let (reviewed, sealed) = build_review(
        &scope,
        vec![image.clone(), notes.clone()],
        project,
        qualifies(1000, 9000, 2, 1000),
        "Explain",
    )?;
    assert_eq!(reviewed.files[0].delivery, Some(AttachmentDelivery::Native));
    assert_eq!(
        reviewed.files[0].media.as_deref(),
        Some(STANDARD.encode(&png).as_str())
    );
    assert_eq!(sealed[0].qualification.as_deref(), Some("verified-profile"));
    assert_eq!(reviewed.files[1].excerpt.as_ref().unwrap().text, "caf");
    assert!(reviewed.files.iter().all(|file| file.blocker.is_none()));
    assert!(reviewed.minimum_encoded_bytes > png.len() as u64);

    let (limited, _) = build_review(
        &scope,
        vec![image.clone(), notes.clone()],
        project,
        qualifies(10, 9000, 1, 1000),
        "Explain",
    )?;
    assert_eq!(
        limited.files[0].delivery,
        Some(AttachmentDelivery::Reference)
    );
    assert!(
        limited.files[0]
            .native_unavailable
            .as_deref()
            .unwrap()
            .contains("qualified")
    );
    assert!(
        limited.files[1]
            .blocker
            .as_deref()
            .unwrap()
            .contains("qualified limit of 1")
    );
    let (encoded, _) = build_review(
        &scope,
        vec![image.clone()],
        project,
        qualifies(1000, 40, 2, 1000),
        "Explain",
    )?;
    assert!(
        encoded.files[0]
            .blocker
            .as_deref()
            .unwrap()
            .contains("encoded request")
    );
    assert!(
        encoded.files[0]
            .alternatives
            .contains(&AttachmentDelivery::Reference)
    );
    let (context, _) = build_review(
        &scope,
        vec![image],
        project,
        qualifies(1000, 9000, 2, 110),
        "Explain",
    )?;
    assert!(
        context.files[0]
            .blocker
            .as_deref()
            .unwrap()
            .contains("context input")
    );
    Ok(())
}
