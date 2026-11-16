use super::{MarkdownKind, MarkdownRange, analyze_markdown};

fn slice(text: &str, range: MarkdownRange) -> &str {
    &text[range.byte_start..range.byte_end]
}

fn code_mirror_units(text: &str) -> usize {
    text.replace("\r\n", "\n").encode_utf16().count()
}

#[test]
fn source_positions_address_the_original_bytes_and_the_exact_editor_revision() {
    let text = "\u{feff}# Café 🙂 [[./notas/Élan#Seção|Atalho]] ##\r\n\r> 1. [X] módulo\r\n>    - [ ] outro\n>    - [🔄] custom\r💬 ==olá== [rota](./docs/map.md#intro)";
    let analysis = analyze_markdown(text, "dirty-19".into());
    assert_eq!(analysis.buffer_revision, "dirty-19");
    let json = serde_json::to_value(&analysis).unwrap();
    assert_eq!(json["bufferRevision"], "dirty-19");

    let mut tasks = Vec::new();
    let mut list_markers = Vec::new();
    for node in analysis.nodes {
        let range = node.range;
        assert_eq!(range.from, code_mirror_units(&text[..range.byte_start]));
        assert_eq!(range.to, code_mirror_units(&text[..range.byte_end]));
        match node.kind {
            MarkdownKind::Heading { level, text: title } => {
                assert_eq!(level, 1);
                assert_eq!(title, "Café 🙂 Atalho");
                assert_eq!(range.from, 1);
                assert_eq!(range.byte_start, 3);
                assert_eq!(
                    node.markers
                        .iter()
                        .map(|&range| slice(text, range))
                        .collect::<Vec<_>>(),
                    ["#", "##"]
                );
            }
            MarkdownKind::Wikilink {
                target,
                alias,
                anchor,
            } => {
                assert_eq!(target, "./notas/Élan");
                assert_eq!(alias.as_deref(), Some("Atalho"));
                assert_eq!(anchor.as_deref(), Some("Seção"));
                assert_eq!(slice(text, range), "[[./notas/Élan#Seção|Atalho]]");
            }
            MarkdownKind::ListItem { marker_range } => list_markers.push(slice(text, marker_range)),
            MarkdownKind::TaskMarker {
                checked,
                status,
                value_range,
            } => {
                assert_eq!(value_range.to - value_range.from, status.len_utf16());
                tasks.push((
                    checked,
                    status,
                    slice(text, range),
                    slice(text, value_range),
                ));
            }
            MarkdownKind::Highlight => {
                assert_eq!(slice(text, range), "==olá==");
                assert_eq!(
                    node.markers
                        .iter()
                        .map(|&range| slice(text, range))
                        .collect::<Vec<_>>(),
                    ["==", "=="]
                );
            }
            MarkdownKind::Link {
                target,
                text: label,
            } => {
                assert_eq!(target, "./docs/map.md#intro");
                assert_eq!(label, "rota");
            }
            _ => {}
        }
    }
    assert_eq!(list_markers, ["1.", "-", "-"]);
    assert_eq!(
        tasks,
        [
            (true, 'X', "[X]", "X"),
            (false, ' ', "[ ]", " "),
            (false, '🔄', "[🔄]", "🔄"),
        ]
    );
    let edited = format!("😀\n{text}");
    let later = analyze_markdown(&edited, "dirty-20".into());
    let first_task = later
        .nodes
        .iter()
        .find(|node| matches!(node.kind, MarkdownKind::TaskMarker { .. }))
        .unwrap();
    assert_eq!(later.buffer_revision, "dirty-20");
    assert_eq!(
        first_task.range.from,
        code_mirror_units(&edited[..edited.find("[X]").unwrap()])
    );
}

#[test]
fn extension_ranges_avoid_literal_code_yaml_comments_and_link_destinations() {
    let text = concat!(
        "---\nlayout: kanban # YAML comment\npath: '[[Fake]] #skip'\n---\n",
        "# Reunião\n\n",
        "> [!tip|small]- Dica *real*\n> Exemplo ![[Guia#^secao-2|Leia aqui]].\n\n",
        "Trecho **importante** ~~antigo~~ e ==novo== #projeto/ação ^secao-2\n\n",
        "`#skip ^skip [[Fake]]` e %% #skip ^skip [[Fake]] %% e \\#skip\n",
        "%%\n#skip [[Fake]]\n%%\n\n",
        "![[#Local]] [nota](https://example.invalid/#skip) email#skip #123\n\n",
        "```dataview\nLIST FROM #skip\n- [ ] literal\n[[Fake]]\n```\n\n",
        "| Result | Value |\n| --- | --- |\n| Número[^nota] | $a^2$ |\n\n",
        "[^nota]: Veja também [[Outra]].\n",
    );
    let analysis = analyze_markdown(text, "syntax".into());
    let mut tags = Vec::new();
    let mut block_ids = Vec::new();
    let mut embeds = Vec::new();
    let mut wikilinks = Vec::new();
    let mut types = Vec::new();
    for node in analysis.nodes {
        match node.kind {
            MarkdownKind::Tag { name } => tags.push(name),
            MarkdownKind::BlockId { id } => block_ids.push(id),
            MarkdownKind::Embed {
                target,
                anchor,
                alias,
            } => embeds.push((target, anchor, alias)),
            MarkdownKind::Wikilink { target, .. } => wikilinks.push(target),
            MarkdownKind::TaskMarker { .. } => panic!("unexpected task marker in literal source"),
            MarkdownKind::Callout {
                callout_type,
                title,
                fold,
                metadata,
            } => {
                assert_eq!(callout_type, "tip");
                assert_eq!(title, "Dica real");
                assert_eq!(fold, Some("collapsed"));
                assert_eq!(metadata.as_deref(), Some("small"));
                types.push("callout");
            }
            MarkdownKind::CodeBlock { language, text } => {
                assert_eq!(language, "dataview");
                assert!(text.contains("LIST FROM #skip"));
                types.push("code");
            }
            MarkdownKind::Footnote { name, definition } => {
                assert_eq!(name, "nota");
                types.push(if definition {
                    "definition"
                } else {
                    "reference"
                });
            }
            MarkdownKind::Math { display, text } => {
                assert!(!display);
                assert_eq!(text, "a^2");
                types.push("math");
            }
            MarkdownKind::Table => types.push("table"),
            MarkdownKind::Comment => types.push("comment"),
            _ => {}
        }
    }
    assert_eq!(tags, ["projeto/ação"]);
    assert_eq!(block_ids, ["secao-2"]);
    assert_eq!(wikilinks, ["Outra"]);
    assert_eq!(
        embeds,
        [
            (
                "Guia".into(),
                Some("^secao-2".into()),
                Some("Leia aqui".into())
            ),
            ("".into(), Some("Local".into()), None),
        ]
    );
    assert_eq!(
        types,
        [
            "callout",
            "comment",
            "comment",
            "code",
            "table",
            "reference",
            "math",
            "definition"
        ]
    );
}

#[test]
fn narrow_ranges_keep_property_comments_tasks_metadata_and_queries_in_place() {
    let text = concat!(
        "\u{feff}---\r\nkanban-plugin: board # keep me\r\nunknown: {flow: yes}\r\n---\r\n\r\n",
        "- [ ] Handle metadata 📅 2026-12-01 [owner:: Cíntia]\r\n",
        "\t- [/] Work in progress\r\n\r\n",
        "```dataview\r\nLIST FROM #work\r\n```\r\n",
        "%% kanban:settings\r\n```\r\n{\"collapse-tags\":true}\r\n```\r\n%%\r\n",
    );
    let analysis = analyze_markdown(text, "edit".into());
    let frontmatter = analysis
        .nodes
        .iter()
        .find_map(|node| match node.kind {
            MarkdownKind::Frontmatter { body_range } => Some((node.range, body_range)),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        slice(text, frontmatter.0),
        "---\r\nkanban-plugin: board # keep me\r\nunknown: {flow: yes}\r\n---"
    );
    assert_eq!(
        slice(text, frontmatter.1),
        "kanban-plugin: board # keep me\r\nunknown: {flow: yes}\r\n"
    );

    let tasks = analysis
        .nodes
        .iter()
        .filter_map(|node| match node.kind {
            MarkdownKind::TaskMarker {
                status,
                value_range,
                ..
            } => Some((status, value_range)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(tasks.len(), 2);
    assert_eq!(tasks[1].0, '/');
    assert_eq!(slice(text, tasks[1].1), "/");
    let task_value = tasks[0].1;
    let mut edited = text.to_owned();
    edited.replace_range(task_value.byte_start..task_value.byte_end, "x");
    assert_eq!(edited, text.replacen("[ ]", "[x]", 1));
}

#[test]
fn escaped_table_pipes_do_not_shift_later_links_or_preview_marks() {
    let text = concat!(
        "> Cabeçalho\r\n> ------\r\n\r\n",
        "| Campo | Outro |\r\n| --- | --- |\r\n",
        "| AA \\| BB [[😄/alvo#Heading\\|Rótulo]] ==😄oi== #tag | [[x\\|Outro]] |\r\n",
        "\r\n[[Note#A\\|B|Label]]\r\n",
    );
    let analysis = analyze_markdown(text, "table".into());
    let mut links = Vec::new();
    let mut decorations = Vec::new();
    for node in analysis.nodes {
        assert_eq!(
            node.range.from,
            code_mirror_units(&text[..node.range.byte_start])
        );
        assert_eq!(
            node.range.to,
            code_mirror_units(&text[..node.range.byte_end])
        );
        match node.kind {
            MarkdownKind::Wikilink {
                target,
                alias,
                anchor,
            } => {
                links.push((target, alias, anchor, slice(text, node.range)));
            }
            MarkdownKind::Highlight | MarkdownKind::Heading { .. } => {
                decorations.extend(node.markers.into_iter().map(|mark| slice(text, mark)));
            }
            MarkdownKind::Tag { name } => assert_eq!(name, "tag"),
            _ => {}
        }
    }
    assert_eq!(
        links,
        [
            (
                "😄/alvo".into(),
                Some("Rótulo".into()),
                Some("Heading".into()),
                "[[😄/alvo#Heading\\|Rótulo]]"
            ),
            ("x".into(), Some("Outro".into()), None, "[[x\\|Outro]]"),
            (
                "Note".into(),
                Some("Label".into()),
                Some("A|B".into()),
                "[[Note#A\\|B|Label]]"
            ),
        ]
    );
    assert_eq!(decorations, ["------", "==", "=="]);
}
