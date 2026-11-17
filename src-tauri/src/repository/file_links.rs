use anyhow::{Context, Result, ensure};
use serde::Serialize;
use uuid::Uuid;

use crate::store::Store;

use super::{
    ProjectCheckout,
    checkout_items::{checkout_regular_file, validate_name},
    documents::relative_name,
    verify_checkout,
};

const PREFIX: &str = "omniagent://open-file?";
const MAX_LINK_BYTES: usize = 32_767;

#[derive(Debug, PartialEq, Eq)]
pub struct FileLink {
    checkout_id: String,
    path: String,
    line: Option<u32>,
    column: Option<u32>,
}

#[derive(Serialize)]
pub struct ResolvedFileLink {
    checkout: ProjectCheckout,
    path: String,
    line: Option<u32>,
    column: Option<u32>,
}

impl FileLink {
    pub fn parse(argument: &str) -> Option<Self> {
        if argument.len() > MAX_LINK_BYTES {
            return None;
        }
        let query = argument.strip_prefix(PREFIX)?;
        let mut checkout_id = None;
        let mut path = None;
        let mut line = None;
        let mut column = None;
        for entry in query.split('&') {
            let (key, raw) = entry.split_once('=')?;
            let value = decode_component(raw)?;
            match key {
                "checkout" if checkout_id.is_none() => {
                    let id = Uuid::parse_str(&value).ok()?;
                    if id.hyphenated().to_string() != value {
                        return None;
                    }
                    checkout_id = Some(value);
                }
                "path" if path.is_none() => {
                    for name in value.split('/') {
                        validate_name(name).ok()?;
                    }
                    path = Some(value);
                }
                "line" if line.is_none() => line = Some(positive_number(&value)?),
                "column" if column.is_none() => column = Some(positive_number(&value)?),
                _ => return None,
            }
        }
        let path = path?;
        if column.is_some() && (line.is_none() || is_note(&path)) {
            return None;
        }
        Some(Self {
            checkout_id: checkout_id?,
            path,
            line,
            column,
        })
    }

    pub async fn resolve(self, store: &Store) -> Result<ResolvedFileLink> {
        let checkout = store
            .registered_checkouts()
            .await?
            .into_iter()
            .find(|checkout| checkout.checkout_id == self.checkout_id)
            .context("The linked checkout is not registered.")?;
        let root = verify_checkout(
            store,
            self.checkout_id,
            checkout.checkout_root.clone().into(),
        )
        .await?
        .root;
        let path = self.path;
        let path = tokio::task::spawn_blocking(move || -> Result<String> {
            let file = checkout_regular_file(&root, &path)?;
            let canonical = dunce::canonicalize(&file)?;
            // Recheck resolved Windows names as well, including legacy short filename aliases.
            relative_name(&root, &canonical)
        })
        .await??;
        ensure!(
            self.column.is_none() || !is_note(&path),
            "Notes open at a line."
        );
        Ok(ResolvedFileLink {
            checkout,
            path,
            line: self.line,
            column: self.column,
        })
    }
}

fn is_note(path: &str) -> bool {
    path.rsplit_once('.').is_some_and(|(_, extension)| {
        extension.eq_ignore_ascii_case("md") || extension.eq_ignore_ascii_case("markdown")
    })
}

fn positive_number(value: &str) -> Option<u32> {
    if !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    value.parse::<u32>().ok().filter(|value| *value > 0)
}

// Decode one URLSearchParams value without accepting malformed escapes or lossy UTF-8.
fn decode_component(raw: &str) -> Option<String> {
    let mut bytes = raw.bytes();
    let mut decoded = Vec::with_capacity(raw.len());
    while let Some(byte) = bytes.next() {
        decoded.push(match byte {
            b'+' => b' ',
            b'%' => {
                let high = char::from(bytes.next()?).to_digit(16)?;
                let low = char::from(bytes.next()?).to_digit(16)?;
                (high * 16 + low) as u8
            }
            b'/' | b'-' | b'.' | b'_' | b'~' | b'*' => byte,
            byte if byte.is_ascii_alphanumeric() => byte,
            _ => return None,
        });
    }
    String::from_utf8(decoded).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "619536f7-bac1-46ea-af56-88cc8b22013a";

    #[test]
    fn parses_scoped_file_and_source_cursor_links() {
        let link = FileLink::parse(&format!(
            "{PREFIX}checkout={ID}&path=src%2FRevis%C3%A3o+%23a.ts&line=7&column=3"
        ))
        .unwrap();
        assert_eq!(
            link,
            FileLink {
                checkout_id: ID.into(),
                path: "src/Revisão #a.ts".into(),
                line: Some(7),
                column: Some(3)
            }
        );
        let note =
            FileLink::parse(&format!("{PREFIX}checkout={ID}&path=docs/Note.MD&line=12")).unwrap();
        assert_eq!((note.line, note.column), (Some(12), None));
    }

    #[test]
    fn rejects_routes_escapes_and_noncanonical_paths() {
        let base = format!("{PREFIX}checkout={ID}&path=");
        for input in [
            "",
            "%2Fsrc.ts",
            "C%3A%2Fa.ts",
            "src%5Ca.ts",
            "src/%2e%2E/a.ts",
            "./a.ts",
            "dir//a.ts",
            "a.ts/",
            "a/%2eGIT/config",
            "a/.git./config",
            "NUL.ts",
            "a%00.ts",
            "%GG.ts",
            "%FF.ts",
            "a%",
            "a.ts#fragment",
            "a.ts&run=agent",
            "a.ts&path=b.ts",
            "a.ts&line=0",
            "a.ts&line=-1",
            "a.ts&line=4294967296",
            "a.ts&line=1&line=2",
            "a.ts&column=1",
            "Note.md&line=1&column=2",
            "a.ts&line=1&column=0",
        ] {
            assert!(
                FileLink::parse(&format!("{base}{input}")).is_none(),
                "{input}"
            );
        }
        for input in [
            "https://open-file",
            "omniagent://user@open-file",
            "omniagent://open-file:80",
            "omniagent://open-file/other",
            "omniagent://delete-file",
        ] {
            assert!(
                FileLink::parse(&format!("{input}?checkout={ID}&path=a.ts")).is_none(),
                "{input}"
            );
        }
        assert!(FileLink::parse(&format!("{PREFIX}checkout=unknown&path=a.ts")).is_none());
    }

    #[tokio::test]
    async fn resolves_only_files_in_the_exact_registered_checkout() -> Result<()> {
        use crate::repository::{git_path, git_query, register};
        use std::fs;

        let temporary = tempfile::tempdir()?;
        let root = temporary.path().join("checkout");
        fs::create_dir(&root)?;
        git_query(&git_path()?, &root, &["init", "--quiet"])?;
        fs::write(root.join("source.ts"), b"export const value = 1;")?;
        let nested = root.join("nested");
        fs::create_dir(&nested)?;
        git_query(&git_path()?, &nested, &["init", "--quiet"])?;
        fs::write(nested.join("source.ts"), b"export const value = 2;")?;
        let store = Store::open(temporary.path().join("checkout.sqlite"))?;
        let checkout = register(&store, root).await?;
        let parse = |id: &str, path: &str| {
            FileLink::parse(&format!("{PREFIX}checkout={id}&path={path}")).unwrap()
        };
        let resolved = parse(&checkout.checkout_id, "source.ts")
            .resolve(&store)
            .await?;
        assert_eq!(resolved.checkout, checkout);
        for path in ["missing.ts", "nested/source.ts", "nested"] {
            assert!(
                parse(&checkout.checkout_id, path)
                    .resolve(&store)
                    .await
                    .is_err()
            );
        }
        assert!(parse(ID, "source.ts").resolve(&store).await.is_err());
        assert!(store.active_checkout().await?.is_none());
        Ok(())
    }
}
