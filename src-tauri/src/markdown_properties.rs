use std::ops::Range;

use anyhow::{Context, Result, ensure};
use yaml_edit::{Mapping, MappingEntry, TextPosition, YamlFile, YamlNode};

#[cfg(test)]
mod tests;

/// Replaces one existing top-level property with the supplied YAML value. The result is an
/// edited buffer, ready for the same revision-checked save as a direct source edit.
pub fn replace_markdown_property(markdown: &str, key: &str, value: &str) -> Result<String> {
    let frontmatter = frontmatter_body(markdown).context("The note has no YAML frontmatter")?;
    let body = &markdown[frontmatter.clone()];
    let yaml: YamlFile = body.parse().context("Invalid YAML frontmatter")?;
    let mapping = root_mapping(&yaml)?;
    let selected = property(&mapping, key)?;
    let old_value = selected
        .value_node()
        .context("The property has no YAML value")?;
    let old_value_range = bytes(old_value.byte_range());
    let old_value_text = &body[old_value_range.clone()];
    let new_value = parse_value(value)?;
    let value = value.trim();
    if old_value_text == value {
        return Ok(markdown.to_owned());
    }

    let key_end = selected
        .key_node()
        .context("The property has no YAML key")?
        .byte_range()
        .end;
    let entry_range = key_end as usize..selected.byte_range().end as usize;
    let comments = yaml
        .comments()
        .map(|comment| comment.text().to_owned())
        .collect::<Vec<_>>();
    mapping.set(key, &new_value);
    let rendered = yaml.to_string();
    let edited = if old_value_text.is_empty() || old_value_text.contains(['\r', '\n']) {
        // Implicit nulls and block values can require moving the value across its own whitespace.
        bounded_entry_edit(body, rendered, entry_range)?
    } else {
        // Splice only the rendered value. Even whitespace outside an inline value stays intact.
        let rendered_value = property(&mapping, key)?
            .value_node()
            .context("The edited property has no YAML value")?;
        let rendered_value = &rendered[bytes(rendered_value.byte_range())];
        let mut edited = body.to_owned();
        edited.replace_range(old_value_range, rendered_value);
        edited
    };
    let after: YamlFile = edited
        .parse()
        .context("The property edit produced invalid YAML")?;
    ensure!(
        after
            .comments()
            .map(|comment| comment.text().to_owned())
            .eq(comments),
        "The YAML edit would change or remove comments"
    );
    let after = property(&root_mapping(&after)?, key)?
        .value_node()
        .context("The edited property has no YAML value")?;
    ensure!(
        after.yaml_eq(&new_value),
        "The property does not contain the requested YAML value"
    );

    let mut result = markdown.to_owned();
    result.replace_range(frontmatter, &edited);
    Ok(result)
}

fn bounded_entry_edit(source: &str, mut edited: String, range: Range<usize>) -> Result<String> {
    let prefix = &source[..range.start];
    let suffix = &source[range.end..];
    let rendered_end = edited
        .len()
        .checked_sub(suffix.len())
        .context("The YAML edit changed neighboring text")?;
    ensure!(
        rendered_end >= prefix.len() && edited.starts_with(prefix) && edited.ends_with(suffix),
        "The YAML edit changed neighboring text"
    );

    let original_break = ending(&source[range]);
    let rendered_break = ending(&edited[prefix.len()..rendered_end]);
    if !original_break.is_empty() && rendered_break != original_break {
        edited.replace_range(
            rendered_end - rendered_break.len()..rendered_end,
            original_break,
        );
    }
    Ok(edited)
}

fn root_mapping(yaml: &YamlFile) -> Result<Mapping> {
    let mut documents = yaml.documents();
    let document = documents.next().context("The YAML frontmatter is empty")?;
    ensure!(
        documents.next().is_none(),
        "The frontmatter contains more than one YAML document"
    );
    document
        .as_mapping()
        .context("The YAML frontmatter must be a mapping")
}

fn property(mapping: &Mapping, key: &str) -> Result<MappingEntry> {
    let mut entries = mapping.entries().filter(|entry| entry.key_matches(key));
    let selected = entries
        .next()
        .context("The property is absent from the YAML frontmatter")?;
    ensure!(
        entries.next().is_none(),
        "The YAML property occurs more than once"
    );
    Ok(selected)
}

fn parse_value(value: &str) -> Result<YamlNode> {
    ensure!(
        !value.contains(['\r', '\n', '\0']),
        "Enter a single YAML value on one line"
    );
    let value = value.trim();
    let source = format!("value: {value}");
    let yaml: YamlFile = source.parse().context("Invalid YAML property value")?;
    let map = root_mapping(&yaml)?;
    let parsed = property(&map, "value")?
        .value_node()
        .context("Enter a YAML property value")?;
    ensure!(
        map.entries().count() == 1 && source.get(bytes(parsed.byte_range())) == Some(value),
        "Enter one YAML value without comments or additional keys"
    );
    Ok(parsed)
}

fn bytes(position: TextPosition) -> Range<usize> {
    position.start as usize..position.end as usize
}

fn frontmatter_body(markdown: &str) -> Option<Range<usize>> {
    let text = markdown.strip_prefix('\u{feff}').unwrap_or(markdown);
    let (first, mut rest) = split_line(text);
    if first.trim_end_matches([' ', '\t']) != "---" {
        return None;
    }
    let start = markdown.len() - rest.len();
    while !rest.is_empty() {
        let (line, tail) = split_line(rest);
        if line.trim_end_matches([' ', '\t']) == "---" {
            return Some(start..markdown.len() - rest.len());
        }
        rest = tail;
    }
    None
}

fn split_line(text: &str) -> (&str, &str) {
    let Some(end) = text.find(['\r', '\n']) else {
        return (text, "");
    };
    let break_len = if text[end..].starts_with("\r\n") {
        2
    } else {
        1
    };
    (&text[..end], &text[end + break_len..])
}

fn ending(text: &str) -> &str {
    ["\r\n", "\n", "\r"]
        .into_iter()
        .find(|end| text.ends_with(end))
        .unwrap_or("")
}
