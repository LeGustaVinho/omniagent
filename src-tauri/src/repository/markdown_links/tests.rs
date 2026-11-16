use super::*;

const SOURCE: &str = "docs/Intro.md";

fn note(path: &str, title: Option<&str>, aliases: &[&str]) -> MarkdownLinkNote {
    MarkdownLinkNote {
        path: path.into(),
        title: title.map(str::to_owned),
        aliases: aliases.iter().map(|alias| (*alias).into()).collect(),
        anchors: vec![],
    }
}

fn wiki(target: &str, anchor: Option<&str>) -> MarkdownLinkQuery {
    MarkdownLinkQuery {
        source_path: SOURCE.into(),
        target: target.into(),
        anchor: anchor.map(str::to_owned),
        syntax: MarkdownLinkSyntax::Wiki,
    }
}

fn location(path: &str, anchor: Option<&str>, line: Option<u32>) -> MarkdownLinkLocation {
    MarkdownLinkLocation {
        path: path.into(),
        anchor: anchor.map(str::to_owned),
        line,
    }
}

#[test]
fn explicit_paths_and_wiki_name_ambiguity_follow_the_phase05_precedence() {
    use MarkdownLinkResolution::*;

    let catalog = [
        note(SOURCE, None, &[]),
        note("docs/nested/Guide.Markdown", Some("Guide?"), &[]),
        note("nested/Guide.md", None, &["Quick review", "Guide"]),
        note("docs/nested/Guide.md", None, &[]),
        note("docs/appendix/Proof.md", None, &[]),
        note("archive/Old.md", Some("Quick Review"), &[]),
    ];
    let resolver = MarkdownLinkResolver::new(&catalog);
    for (target, expected) in [
        ("/NESTED/guide", "nested/Guide.md"),
        ("nested/Guide", "nested/Guide.md"),
        ("./nested/Guide.Md", "docs/nested/Guide.md"),
        ("..\\Nested\\guide.md", "nested/Guide.md"),
        ("appendix/Proof", "docs/appendix/Proof.md"),
        ("Guide?", "docs/nested/Guide.Markdown"),
    ] {
        assert_eq!(
            resolver.resolve(&wiki(target, None)),
            Resolved {
                location: location(expected, None, None)
            }
        );
    }
    assert_eq!(
        resolver.resolve(&wiki("./nested/Guide", None)),
        Ambiguous {
            candidates: vec![
                location("docs/nested/Guide.Markdown", None, None),
                location("docs/nested/Guide.md", None, None),
            ]
        }
    );
    assert_eq!(
        resolver.resolve(&wiki("QUICK REVIEW", None)),
        Ambiguous {
            candidates: vec![
                location("archive/Old.md", None, None),
                location("nested/Guide.md", None, None),
            ]
        }
    );
    assert_eq!(
        resolver.resolve(&wiki("guide.md", None)),
        Ambiguous {
            candidates: vec![
                location("docs/nested/Guide.Markdown", None, None),
                location("docs/nested/Guide.md", None, None),
                location("nested/Guide.md", None, None),
            ]
        }
    );
    assert_eq!(resolver.resolve(&wiki("obsolete", None)), Missing);
}

#[test]
fn follows_accented_headings_and_literal_block_ids_without_using_anchors_to_pick_a_note() {
    use MarkdownLinkAnchorKind::*;
    use MarkdownLinkResolution::*;

    let mut intro = note(SOURCE, None, &[]);
    intro.anchors = vec![MarkdownLinkAnchor {
        kind: Heading,
        label: "Context".into(),
        line: 2,
    }];
    let mut cafe = note(
        "docs/Café.md",
        Some("Revisão"),
        &["Referência", "Same note name"],
    );
    cafe.anchors = vec![
        MarkdownLinkAnchor {
            kind: Heading,
            label: "Ｎúmero do Café".into(),
            line: 25,
        },
        MarkdownLinkAnchor {
            kind: Block,
            label: "^Owner-1".into(),
            line: 15,
        },
        MarkdownLinkAnchor {
            kind: Heading,
            label: "Número do Café".into(),
            line: 6,
        },
    ];
    let other = note("docs/Cafe.md", None, &["Same note name"]);
    let catalog = [cafe, other, intro];
    let resolver = MarkdownLinkResolver::new(&catalog);
    let queries = [
        wiki("CAFE\u{301}", Some("~NÚMERO---Do *Cafe\u{301}*")),
        wiki("Cafe", None),
        wiki("Referência", Some("^Owner-1")),
        wiki("Revisão", Some("^owner-1")),
        wiki("", Some("context")),
        wiki("Same note name", Some("Número do Café")),
    ];
    let expected = [
        Resolved {
            location: location("docs/Café.md", Some("Número do Café"), Some(6)),
        },
        Resolved {
            location: location("docs/Cafe.md", None, None),
        },
        Resolved {
            location: location("docs/Café.md", Some("^Owner-1"), Some(15)),
        },
        MissingAnchor {
            location: location("docs/Café.md", Some("^owner-1"), None),
        },
        Resolved {
            location: location(SOURCE, Some("Context"), Some(2)),
        },
        Ambiguous {
            candidates: vec![
                location("docs/Cafe.md", Some("Número do Café"), None),
                location("docs/Café.md", Some("Número do Café"), Some(6)),
            ],
        },
    ];
    assert_eq!(resolver.resolve_batch(&queries), expected);
    let mut reversed = catalog;
    reversed.reverse();
    assert_eq!(
        MarkdownLinkResolver::new(&reversed).resolve_batch(&queries),
        expected
    );

    let wire = serde_json::to_value(&expected[3]).unwrap();
    assert_eq!(wire["status"], "missing_anchor");
    assert_eq!(wire["location"]["path"], "docs/Café.md");
    let from_frontend: MarkdownLinkQuery = serde_json::from_value(serde_json::json!({
        "sourcePath": SOURCE, "target": "Café"
    }))
    .unwrap();
    assert_eq!(from_frontend.syntax, MarkdownLinkSyntax::Wiki);
    assert_eq!(from_frontend.anchor, None);
}

#[test]
fn markdown_urls_are_note_relative_and_media_never_enters_the_note_graph() {
    use MarkdownLinkResolution::*;

    let mut encoded = note("docs/Fiche #1.md", None, &[]);
    encoded.anchors = vec![MarkdownLinkAnchor {
        kind: MarkdownLinkAnchorKind::Heading,
        label: "Données".into(),
        line: 4,
    }];
    let catalog = [
        note(SOURCE, None, &[]),
        note("docs/Handbook.Markdown", None, &[]),
        note("Handbook.Markdown", None, &[]),
        note("scan.pdf.md", None, &[]),
        note("docs/Map.canvas", None, &[]),
        note("docs/Overview.base", None, &[]),
        encoded,
    ];
    let resolver = MarkdownLinkResolver::new(&catalog);
    let markdown = |target| MarkdownLinkQuery::from_markdown(SOURCE, target).unwrap();
    assert_eq!(
        resolver.resolve(&markdown("Handbook.Markdown")),
        Resolved {
            location: location("docs/Handbook.Markdown", None, None)
        }
    );
    assert_eq!(
        resolver.resolve(&markdown("/Handbook.Markdown")),
        Resolved {
            location: location("Handbook.Markdown", None, None)
        }
    );
    assert_eq!(
        resolver.resolve(&markdown("Fiche%20%231.md#Donn%C3%A9es")),
        Resolved {
            location: location("docs/Fiche #1.md", Some("Données"), Some(4))
        }
    );
    assert_eq!(resolver.resolve(&markdown("Map.canvas")), Missing);
    assert_eq!(resolver.resolve(&wiki("Overview.base", None)), Missing);
    assert_eq!(
        resolver.resolve(&wiki("scan.pdf", None)),
        Attachment {
            paths: vec!["docs/scan.pdf".into(), "scan.pdf".into()]
        }
    );
    assert_eq!(
        resolver.resolve(&wiki("Media/Clip.MP4", None)),
        Attachment {
            paths: vec!["docs/Media/Clip.MP4".into(), "Media/Clip.MP4".into()]
        }
    );
    assert_eq!(
        resolver.resolve(&markdown("Media/Clip.MP4")),
        Attachment {
            paths: vec!["docs/Media/Clip.MP4".into()]
        }
    );
    assert_eq!(
        resolver.resolve(&wiki("../Review.pdf", None)),
        Attachment {
            paths: vec!["Review.pdf".into()]
        }
    );
    assert_eq!(
        resolver.resolve(&wiki("/Media/Guide.PNG", None)),
        Attachment {
            paths: vec!["Media/Guide.PNG".into()]
        }
    );
    for bad_url in [
        "https://example.com/notes.md",
        "file:///C:/local.md",
        "//server/share/Readme.md",
        "malformed-%E2.md",
    ] {
        assert!(
            MarkdownLinkQuery::from_markdown(SOURCE, bad_url).is_none(),
            "{bad_url}"
        );
    }
}

#[test]
fn refuses_unsafe_paths_and_reports_windows_case_collisions() {
    use MarkdownLinkResolution::*;

    let catalog = [
        note(SOURCE, None, &[]),
        note("docs/ReadMe.md", None, &[]),
        note("docs/readme.md", None, &[]),
        note("../escape.md", None, &[]),
        note(".git/private.md", None, &[]),
        note("docs/Unsafe.md:stream", None, &[]),
    ];
    let resolver = MarkdownLinkResolver::new(&catalog);
    assert_eq!(
        resolver.resolve(&wiki("/docs/README.md", None)),
        Ambiguous {
            candidates: vec![
                location("docs/ReadMe.md", None, None),
                location("docs/readme.md", None, None),
            ]
        }
    );
    assert!(resolver.suggest(SOURCE, "readme", 10).is_empty());
    for target in [
        "../../private.md",
        "../../private.jpg",
        "\\\\host\\share\\Guide.md",
        "/.git/private.md",
        "/.GIT/file.pdf",
        "C:private.md",
        "docs/Unsafe.md:stream",
        "bad\0name.md",
    ] {
        assert_eq!(resolver.resolve(&wiki(target, None)), Invalid, "{target}");
    }
    assert_eq!(
        resolver.resolve(
            &MarkdownLinkQuery::from_markdown(SOURCE, "%2e%2e/%2e%2e/private.md").unwrap()
        ),
        Invalid
    );
    assert_eq!(
        resolver.resolve(&MarkdownLinkQuery {
            source_path: "../invalid.md".into(),
            ..wiki("Guide", None)
        }),
        Invalid
    );
}

#[test]
fn completions_use_canonical_paths_and_earliest_anchored_occurrences() {
    use MarkdownLinkAnchorKind::*;

    let mut file = note("docs/Guide #1.md", Some("Setup"), &["Quick start"]);
    file.anchors = vec![
        MarkdownLinkAnchor {
            kind: Heading,
            label: "Deploy Process".into(),
            line: 40,
        },
        MarkdownLinkAnchor {
            kind: Heading,
            label: "Deploy process".into(),
            line: 8,
        },
        MarkdownLinkAnchor {
            kind: Block,
            label: "agent-1".into(),
            line: 12,
        },
    ];
    let catalog = [
        note("docs/nested/Glossary.md", None, &[]),
        note("docs2/Not here.md", None, &[]),
        file,
    ];
    let resolver = MarkdownLinkResolver::new(&catalog);
    let alias = resolver.suggest(SOURCE, "quick", 10);
    assert_eq!(alias.len(), 1);
    assert_eq!(alias[0].target, "/docs/Guide \\#1.md");
    assert_eq!(alias[0].label, "Setup");
    assert_eq!(
        resolver
            .suggest(SOURCE, "/docs/", 10)
            .iter()
            .map(|item| item.path.as_str())
            .collect::<Vec<_>>(),
        ["docs/Guide #1.md", "docs/nested/Glossary.md"]
    );
    assert_eq!(
        resolver.suggest(SOURCE, "./nested/", 10)[0].path,
        "docs/nested/Glossary.md"
    );
    let sections = resolver.suggest(SOURCE, "Quick start#deP", 10);
    assert_eq!(sections.len(), 1);
    assert_eq!(sections[0].target, "/docs/Guide \\#1.md#Deploy process");
    assert_eq!(sections[0].line, Some(8));
    assert_eq!(
        resolver.suggest(SOURCE, "/docs/Guide \\#1.md#deP", 10),
        sections
    );
    assert!(
        resolver
            .suggest(SOURCE, "Quick start|Custom text", 10)
            .is_empty()
    );
    let block = resolver.suggest("docs/Guide #1.md", "#^ag", 10);
    assert_eq!(block[0].target, "/docs/Guide \\#1.md#^agent-1");
    assert_eq!(block[0].line, Some(12));
    assert!(resolver.suggest(SOURCE, "../../", 10).is_empty());
    assert_eq!(resolver.suggest(SOURCE, "", 1).len(), 1);
}
