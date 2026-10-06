const TEXT_EXTENSIONS: [&str; 8] = ["opf", "xhtml", "html", "htm", "xml", "css", "ncx", "svg"];

pub struct Rename {
    pub from: String,
    pub to: String,
    pub media_type: &'static str,
}

pub fn is_text_document(name: &str) -> bool {
    name.rsplit_once('.').is_some_and(|(_, extension)| {
        TEXT_EXTENSIONS
            .iter()
            .any(|candidate| extension.eq_ignore_ascii_case(candidate))
    })
}

/// Rewrites references in `document_name` that point at renamed archive entries.
pub fn rewrite(document_name: &str, text: &str, renames: &[Rename]) -> String {
    let directory = document_name.rsplit_once('/').map_or("", |(directory, _)| directory);
    let is_package = document_name.to_ascii_lowercase().ends_with(".opf");
    let mut text = text.to_owned();

    for rename in renames {
        let old = relative_path(directory, &rename.from);
        let new = relative_path(directory, &rename.to);
        let encoded = (old.replace(' ', "%20"), new.replace(' ', "%20"));
        for (old, new) in [(old, new), encoded] {
            if text.contains(&old) {
                text = replace_reference(&text, &old, &new);
                if is_package {
                    text = update_media_type(&text, &new, rename.media_type);
                }
            }
        }
    }
    text
}

fn relative_path(from_directory: &str, target: &str) -> String {
    let from: Vec<&str> = from_directory.split('/').filter(|part| !part.is_empty()).collect();
    let to: Vec<&str> = target.split('/').collect();
    let common = from
        .iter()
        .zip(&to)
        .take_while(|(a, b)| a == b)
        .count()
        .min(to.len() - 1);

    let mut parts = vec![".."; from.len() - common];
    parts.extend(&to[common..]);
    parts.join("/")
}

/// Replaces `old` only where it forms a whole reference, not part of a longer path.
fn replace_reference(text: &str, old: &str, new: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut last = 0;
    for (start, _) in text.match_indices(old) {
        let end = start + old.len();
        let starts_cleanly = text[..start]
            .trim_end_matches("./")
            .chars()
            .next_back()
            .is_none_or(|c| matches!(c, '"' | '\'' | '(') || c.is_whitespace());
        let ends_cleanly = text[end..]
            .chars()
            .next()
            .is_none_or(|c| matches!(c, '"' | '\'' | ')' | '#' | '?') || c.is_whitespace());
        if starts_cleanly && ends_cleanly {
            output.push_str(&text[last..start]);
            output.push_str(new);
            last = end;
        }
    }
    output.push_str(&text[last..]);
    output
}

fn update_media_type(package: &str, href: &str, media_type: &str) -> String {
    let mut output = String::with_capacity(package.len());
    let mut rest = package;
    while let Some(start) = rest.find("<item") {
        let end = rest[start..]
            .find('>')
            .map_or(rest.len(), |offset| start + offset + 1);
        let tag = &rest[start..end];
        output.push_str(&rest[..start]);
        if has_attribute_value(tag, href) {
            output.push_str(&set_media_type(tag, media_type));
        } else {
            output.push_str(tag);
        }
        rest = &rest[end..];
    }
    output.push_str(rest);
    output
}

fn has_attribute_value(tag: &str, value: &str) -> bool {
    ["", "./"].iter().any(|prefix| {
        tag.contains(&format!("\"{prefix}{value}\"")) || tag.contains(&format!("'{prefix}{value}'"))
    })
}

fn set_media_type(tag: &str, media_type: &str) -> String {
    let Some(attribute) = tag.find("media-type") else {
        return tag.to_owned();
    };
    let Some(open) = tag[attribute..].find(['"', '\'']) else {
        return tag.to_owned();
    };
    let value_start = attribute + open + 1;
    let quote = tag.as_bytes()[value_start - 1] as char;
    let Some(length) = tag[value_start..].find(quote) else {
        return tag.to_owned();
    };
    format!(
        "{}{media_type}{}",
        &tag[..value_start],
        &tag[value_start + length..]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rename() -> Vec<Rename> {
        vec![Rename {
            from: "OEBPS/image/p 1.png".into(),
            to: "OEBPS/image/p 1.jpg".into(),
            media_type: "image/jpeg",
        }]
    }

    #[test]
    fn computes_relative_paths() {
        assert_eq!(relative_path("OEBPS/text", "OEBPS/image/a.png"), "../image/a.png");
        assert_eq!(relative_path("OEBPS", "OEBPS/image/a.png"), "image/a.png");
        assert_eq!(relative_path("", "OEBPS/a.png"), "OEBPS/a.png");
    }

    #[test]
    fn rewrites_xhtml_references_but_not_other_paths() {
        let xhtml = r#"<img src="../image/p%201.png"/><img src="../other/image/p%201.png"/>"#;
        let rewritten = rewrite("OEBPS/text/c.xhtml", xhtml, &rename());
        assert_eq!(
            rewritten,
            r#"<img src="../image/p%201.jpg"/><img src="../other/image/p%201.png"/>"#
        );
    }

    #[test]
    fn rewrites_manifest_href_and_media_type() {
        let opf = r#"<item id="i" href="image/p 1.png" media-type="image/png"/><item id="c" href="c.css" media-type="text/css"/>"#;
        let rewritten = rewrite("OEBPS/content.opf", opf, &rename());
        assert_eq!(
            rewritten,
            r#"<item id="i" href="image/p 1.jpg" media-type="image/jpeg"/><item id="c" href="c.css" media-type="text/css"/>"#
        );
    }
}
