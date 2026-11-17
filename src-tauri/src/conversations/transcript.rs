use std::{fmt::Write as _, io::Write as _, path::Path};

use anyhow::{Context, Result, ensure};
use comrak::{Arena, Options, format_commonmark, nodes::NodeValue, parse_document};
use rusqlite::{Connection, OptionalExtension, params};

use crate::{providers::composer_customizations::display_skill_invocations, store::Store};

use super::{
    Conversation, MessageRevision, MessageRole, context_entries, load_conversation, load_messages,
};

impl Store {
    pub async fn transcript_export_title(
        &self,
        checkout_id: String,
        conversation_id: String,
    ) -> Result<String> {
        self.execute(move |db| {
            Ok(exportable_conversation(db, &checkout_id, &conversation_id)?.title)
        })
        .await
    }

    pub async fn export_conversation_transcript(
        &self,
        checkout_id: String,
        conversation_id: String,
    ) -> Result<String> {
        let (title, messages) = self
            .execute(move |db| {
                let conversation = exportable_conversation(db, &checkout_id, &conversation_id)?;
                let entries = context_entries(db, &conversation.id, &conversation.context_id)?;
                let messages = load_messages(db, &conversation.id, &entries)?;
                Ok((conversation.title, messages))
            })
            .await?;
        tokio::task::spawn_blocking(move || markdown(&title, &messages)).await?
    }
}

fn exportable_conversation(
    db: &Connection,
    checkout_id: &str,
    conversation_id: &str,
) -> Result<Conversation> {
    db.query_row(
        "SELECT 1 FROM conversations WHERE id = ?1 AND checkout_id = ?2 AND background = 0",
        params![conversation_id, checkout_id],
        |_| Ok(()),
    )
    .optional()?
    .context("This conversation is unavailable in the selected worktree.")?;
    load_conversation(db, conversation_id)
}

pub fn default_export_name(title: &str) -> String {
    let name: String = title
        .chars()
        .take(72)
        .map(|ch| {
            if ch.is_control() || matches!(ch, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*')
            {
                ' '
            } else {
                ch
            }
        })
        .collect();
    let title = name.trim_matches([' ', '.']);
    if title.is_empty() {
        "Conversation.md".into()
    } else {
        format!("Conversation - {title}.md")
    }
}

pub fn save_markdown(destination: &Path, text: &str) -> Result<()> {
    let target = if destination.extension().is_none() {
        destination.with_extension("md")
    } else {
        destination.to_owned()
    };
    ensure!(
        target
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("md")
                || extension.eq_ignore_ascii_case("markdown")),
        "Choose a filename ending in .md or .markdown."
    );
    let parent = target
        .parent()
        .context("Choose a folder for the exported conversation.")?;
    let mut staged = tempfile::NamedTempFile::new_in(parent)?;
    staged.write_all(text.as_bytes())?;
    staged.as_file().sync_all()?;
    match staged.persist_noclobber(target) {
        Ok(_) => Ok(()),
        Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
            anyhow::bail!(
                "That file already exists. Choose a new filename to export the conversation."
            );
        }
        Err(error) => Err(error.into()),
    }
}

fn markdown(title: &str, messages: &[MessageRevision]) -> Result<String> {
    let mut text = String::from("# ");
    for ch in title
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
    {
        if ch.is_ascii_punctuation() {
            text.push('\\');
        }
        text.push(ch);
    }
    text.push('\n');
    for message in messages {
        match message.role {
            MessageRole::User => {
                let body = display_skill_invocations(&message.text);
                // User messages are plain text in the transcript, including Markdown punctuation.
                let fence = "`".repeat(
                    body.split(|ch| ch != '`')
                        .map(str::len)
                        .max()
                        .unwrap_or(0)
                        .saturating_add(1)
                        .max(3),
                );
                write!(text, "\n## You\n\n{fence}\n{body}")?;
                if !body.ends_with('\n') {
                    text.push('\n');
                }
                writeln!(text, "{fence}")?;
            }
            MessageRole::Assistant => {
                text.push_str("\n## Assistant\n\n");
                text.push_str(&visible_markdown(&message.text)?);
            }
        }
    }
    Ok(text)
}

fn visible_markdown(text: &str) -> Result<String> {
    let mut options = Options::default();
    options.extension.autolink = true;
    options.extension.footnotes = true;
    options.extension.strikethrough = true;
    options.extension.table = true;
    options.extension.tasklist = true;
    let arena = Arena::new();
    let document = parse_document(&arena, text, &options);
    for node in document.descendants().collect::<Vec<_>>() {
        let mut ast = node.data_mut();
        match &mut ast.value {
            NodeValue::HtmlBlock(_) | NodeValue::HtmlInline(_) => node.detach(),
            NodeValue::Link(link) => {
                link.title.clear();
                if !visible_destination(&link.url) {
                    while let Some(child) = node.first_child() {
                        node.insert_before(child);
                    }
                    node.detach();
                }
            }
            NodeValue::Image(image) => {
                // Match the transcript's text-only image treatment in the exported document.
                let mut label = String::new();
                for child in node.children().flat_map(|child| child.descendants()) {
                    match &child.data().value {
                        NodeValue::Text(text) => label.push_str(text),
                        NodeValue::Code(code) => label.push_str(&code.literal),
                        NodeValue::SoftBreak | NodeValue::LineBreak => label.push(' '),
                        _ => {}
                    }
                }
                if label.is_empty() {
                    label.push_str("Image");
                }
                if !image.url.is_empty() && visible_destination(&image.url) {
                    write!(label, " ({})", image.url)?;
                }
                while let Some(child) = node.first_child() {
                    child.detach();
                }
                ast.value = NodeValue::Text(label.into());
            }
            _ => {}
        }
    }
    let mut output = String::new();
    format_commonmark(document, &options, &mut output)?;
    Ok(output)
}

fn visible_destination(url: &str) -> bool {
    let Some((protocol, _)) = url.split_once(':') else {
        return true;
    };
    protocol.contains(['/', '#', '?'])
        || ["http", "https", "irc", "ircs", "mailto", "xmpp"]
            .iter()
            .any(|allowed| protocol.eq_ignore_ascii_case(allowed))
}

#[cfg(test)]
mod tests {
    use std::{fs, process::Command};

    use super::*;
    use crate::{
        conversations::{
            ConversationLibraryChange, activate_context, insert_conversation, insert_message,
        },
        protection, repository,
    };

    #[tokio::test]
    async fn exports_only_the_active_transcript_from_the_exact_foreground_checkout() -> Result<()> {
        let fixture = tempfile::tempdir()?;
        let root = fixture.path().join("project");
        fs::create_dir(&root)?;
        assert!(
            Command::new("git")
                .args(["init", "--quiet"])
                .current_dir(&root)
                .status()?
                .success()
        );
        let store = Store::open(fixture.path().join("store.sqlite3"))?;
        let checkout = repository::register(&store, root).await?;
        let checkout_id = checkout.checkout_id.clone();
        let (conversation, background) = store.execute(move |db| {
            let chat = insert_conversation(db, checkout_id.clone(), "Decisions #1".into(), false)?;
            let background = insert_conversation(db, checkout_id, "Internal work".into(), true)?;
            let prompt = format!("Need a plan.\t@skill:{}:{}\n```", "a".repeat(64), "b".repeat(64));
            let user = insert_message(db, &chat.id, MessageRole::User, protection::protect(prompt.as_bytes())?)?;
            let old = insert_message(db, &chat.id, MessageRole::Assistant, protection::protect(b"Rewound assistant response")?)?;
            let reply = insert_message(db, &chat.id, MessageRole::Assistant, protection::protect(
                b"**Saved answer**\n\n<!-- invisible content -->\n\n[visible label](javascript:secret) and [site](https://example.invalid \"hidden title\").\n\n[![layout](https://img.example.invalid/layout.png)](https://example.invalid)\n\n```rs\nlet x = 1;\n```",
            )?)?;
            activate_context(db, &chat.id, &[user.clone(), old])?;
            activate_context(db, &chat.id, &[user, reply])?;
            Ok((chat, background))
        }).await?;
        store
            .organize_conversation(
                checkout.checkout_id.clone(),
                conversation.id.clone(),
                ConversationLibraryChange::Archive { archived: true },
            )
            .await?;
        assert!(
            store
                .export_conversation_transcript("wrong-checkout".into(), conversation.id.clone())
                .await
                .is_err()
        );
        assert!(
            store
                .transcript_export_title(checkout.checkout_id.clone(), background.id)
                .await
                .is_err()
        );
        let text = store
            .export_conversation_transcript(checkout.checkout_id, conversation.id)
            .await?;
        for expected in [
            "# Decisions \\#1",
            "## You",
            "````\nNeed a plan.\t[Skill invocation]",
            "## Assistant",
            "**Saved answer**",
            "visible label",
            "https://example.invalid",
            "https://img.example.invalid/layout.png",
            "let x = 1;",
        ] {
            assert!(text.contains(expected), "missing {expected:?} in {text:?}");
        }
        for hidden in [
            "@skill:",
            "Rewound assistant response",
            "invisible content",
            "javascript:secret",
            "hidden title",
            "![layout]",
        ] {
            assert!(!text.contains(hidden), "leaked {hidden:?}");
        }
        Ok(())
    }

    #[test]
    fn saves_markdown_atomically_without_clobbering_existing_content() -> Result<()> {
        let fixture = tempfile::tempdir()?;
        let path = fixture.path().join("Conversation");
        save_markdown(&path, "# Olá\n")?;
        assert!(save_markdown(&path, "replacement").is_err());
        assert_eq!(fs::read_to_string(path.with_extension("md"))?, "# Olá\n");
        assert!(save_markdown(&path.with_extension("exe"), "irrelevant extension").is_err());
        assert_eq!(fs::read_dir(fixture.path())?.count(), 1);
        assert_eq!(
            default_export_name("../CON:\nTítulo"),
            "Conversation - CON  Título.md"
        );
        Ok(())
    }
}
