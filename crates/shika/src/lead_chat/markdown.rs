//! Original, deliberately small markdown renderer. No HTML, images, remote
//! content, terminal escapes, syntax highlighting, or executable controls.
use crate::appearance::Chrome;
use gpui::{FontStyle, FontWeight, HighlightStyle, UnderlineStyle, px};
use std::ops::Range;

#[derive(Clone, Debug, Default)]
pub struct Span {
    pub range: Range<usize>,
    pub bold: bool,
    pub italic: bool,
    pub code: bool,
    pub link: Option<String>,
}
#[derive(Clone, Debug)]
pub struct Block {
    pub continuation: bool,
    pub text: String,
    pub code: bool,
    pub indent: usize,
    pub spans: Vec<Span>,
}

pub fn parse(text: &str, markdown: bool) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut fence: Option<(char, usize)> = None;
    for line in text.lines() {
        let line: String = line
            .chars()
            .filter(|c| !c.is_control() || *c == '\t')
            .collect();
        let trimmed = line.trim_start();
        let marker = trimmed.chars().next().filter(|c| *c == '`' || *c == '~');
        let count = marker.map_or(0, |m| trimmed.chars().take_while(|c| *c == m).count());
        if markdown && count >= 3 {
            if fence.is_none() {
                fence = marker.map(|m| (m, count));
                continue;
            }
            if fence.is_some_and(|(m, n)| marker == Some(m) && count >= n) {
                fence = None;
                continue;
            }
        }
        let code = fence.is_some();
        if line.is_empty() && !code && markdown {
            continue;
        }
        let indent = if markdown && !code {
            ((line.len() - trimmed.len()) / 2).min(1)
        } else {
            0
        };
        let mut line = if markdown && !code {
            trimmed.to_string()
        } else {
            line
        };
        let heading = markdown && !code && line.starts_with('#');
        if heading {
            line = line.trim_start_matches('#').trim_start().into();
        }
        if markdown && !code {
            if let Some(rest) = line
                .strip_prefix("- ")
                .or_else(|| line.strip_prefix("* "))
                .or_else(|| line.strip_prefix("+ "))
            {
                line = format!("• {rest}");
            }
            line = line
                .replace("• [x] ", "✓ ")
                .replace("• [X] ", "✓ ")
                .replace("• [ ] ", "☐ ");
        }
        let (text, mut spans) = if markdown && !code {
            inline(&line, 0)
        } else {
            (line, Vec::new())
        };
        if heading {
            spans.push(Span {
                range: 0..text.len(),
                bold: true,
                ..Default::default()
            });
        }
        // Paragraph/line blocks let the virtual list paint even a very tall
        // response without constructing its off-screen children.
        blocks.push(Block {
            text,
            code,
            continuation: false,
            indent,
            spans,
        });
    }
    if blocks.is_empty() {
        blocks.push(Block {
            text: String::new(),
            code: false,
            continuation: false,
            indent: 0,
            spans: Vec::new(),
        });
    }
    blocks.into_iter().flat_map(split_block).collect()
}
fn split_block(block: Block) -> Vec<Block> {
    if block.code || block.text.len() <= 4096 {
        return vec![block];
    }
    let mut output = Vec::new();
    let mut start = 0;
    while start < block.text.len() {
        let mut end = (start + 4096).min(block.text.len());
        while !block.text.is_char_boundary(end) {
            end -= 1;
        }
        if end < block.text.len()
            && let Some((index, ch)) = block.text[start..end]
                .char_indices()
                .rev()
                .find(|(i, c)| *i > 2048 && c.is_whitespace())
        {
            end = start + index + ch.len_utf8();
        }
        let spans = block
            .spans
            .iter()
            .filter_map(|span| {
                let begin = span.range.start.max(start);
                let finish = span.range.end.min(end);
                if begin >= finish {
                    return None;
                }
                let mut span = span.clone();
                span.range = begin - start..finish - start;
                Some(span)
            })
            .collect();
        output.push(Block {
            text: block.text[start..end].into(),
            code: false,
            continuation: start > 0,
            indent: block.indent,
            spans,
        });
        start = end;
    }
    output
}
fn inline(input: &str, depth: usize) -> (String, Vec<Span>) {
    if depth > 3 {
        return (input.into(), Vec::new());
    }
    let mut text = String::new();
    let mut spans = Vec::new();
    let mut offset = 0;
    while offset < input.len() {
        let rest = &input[offset..];
        if let Some(tail) = rest.strip_prefix('\\')
            && let Some(ch) = tail.chars().next()
        {
            text.push(ch);
            offset += 1 + ch.len_utf8();
            continue;
        }
        if rest.starts_with('[')
            && let Some(end) = rest.find("](")
            && let Some(close) = rest[end + 2..].find(')')
        {
            let target = &rest[end + 2..end + 2 + close];
            if safe_link(target) {
                let start = text.len();
                text.push_str(&rest[1..end]);
                spans.push(Span {
                    range: start..text.len(),
                    link: Some(target.into()),
                    ..Default::default()
                });
                offset += end + 3 + close;
                continue;
            }
        }
        let marker = ["**", "__", "`", "*", "_"]
            .into_iter()
            .find(|m| rest.starts_with(m));
        if let Some(marker) = marker
            && let Some(end) = rest[marker.len()..].find(marker)
        {
            let raw = &rest[marker.len()..marker.len() + end];
            let start = text.len();
            let (inner, mut nested) = if marker == "`" {
                (raw.into(), Vec::new())
            } else {
                inline(raw, depth + 1)
            };
            text.push_str(&inner);
            for span in &mut nested {
                span.range.start += start;
                span.range.end += start;
            }
            spans.extend(nested);
            spans.push(Span {
                range: start..text.len(),
                bold: marker.len() == 2,
                italic: marker == "*" || marker == "_",
                code: marker == "`",
                link: None,
            });
            offset += marker.len() * 2 + end;
            continue;
        }
        let ch = rest.chars().next().expect("nonempty");
        text.push(ch);
        offset += ch.len_utf8();
    }
    // Plain web URLs remain complete and clickable.
    let mut base = 0;
    for word in text.split_inclusive(char::is_whitespace) {
        let target = word.trim_end().trim_end_matches(['.', ',', ';', ')']);
        if safe_link(target)
            && !spans
                .iter()
                .any(|s| s.link.is_some() && s.range.contains(&base))
        {
            spans.push(Span {
                range: base..base + target.len(),
                link: Some(target.into()),
                ..Default::default()
            });
        }
        base += word.len();
    }
    (text, spans)
}
/// The first line of a final reply as plain text for the Lead's card: markdown
/// parsed away (so no `**`, backticks, or heading marks), whitespace collapsed,
/// and cut to a length the card truncates anyway. None for an empty reply.
pub fn reply_first_line(text: &str) -> Option<String> {
    parse(text, true)
        .iter()
        .map(|block| block.text.split_whitespace().collect::<Vec<_>>().join(" "))
        .find(|line| !line.is_empty())
        .map(|line| line.chars().take(200).collect())
}
pub fn safe_link(target: &str) -> bool {
    let valid_scheme = target.starts_with("https://")
        || target.starts_with("http://")
        || target.starts_with("mailto:");
    valid_scheme
        && !target.chars().any(|c| c.is_whitespace() || c.is_control())
        && shika_terminal::openable_uri(target)
}
pub fn highlights(block: &Block, chrome: Chrome) -> Vec<(Range<usize>, HighlightStyle)> {
    block
        .spans
        .iter()
        .map(|span| {
            (
                span.range.clone(),
                HighlightStyle {
                    font_weight: span.bold.then_some(FontWeight::SEMIBOLD),
                    font_style: span.italic.then_some(FontStyle::Italic),
                    background_color: None,
                    underline: span.link.as_ref().map(|_| UnderlineStyle {
                        thickness: px(1.),
                        color: Some(chrome.ink_1.into()),
                        wavy: false,
                    }),
                    ..Default::default()
                },
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn long_prose_is_sliced_without_truncating_text_or_style_ranges() {
        let body = "aé🙂 ".repeat(3000);
        let source = format!("**{body}**");
        let blocks = parse(&source, true);
        assert!(blocks.len() > 1);
        assert!(blocks.iter().all(|b| b.text.len() <= 4096));
        assert_eq!(
            blocks.iter().map(|b| b.text.as_str()).collect::<String>(),
            body
        );
        assert!(blocks.iter().skip(1).all(|b| b.continuation));
        assert!(blocks.iter().all(|b| {
            b.spans.iter().all(|span| {
                b.text.is_char_boundary(span.range.start) && b.text.is_char_boundary(span.range.end)
            })
        }));
    }
    #[test]
    fn final_bash_is_code_and_html_is_only_text() {
        let blocks = parse(
            "## Done\n**Strong** and *quiet* `code`\n```bash\nprintf 'ok'\n```\n<script>never run</script>\n[bad](javascript:alert)\n[PR](https://example.com/p/1)",
            true,
        );
        assert!(blocks[2].code);
        assert_eq!(blocks[2].text, "printf 'ok'");
        assert_eq!(blocks[3].text, "<script>never run</script>");
        assert!(!safe_link("javascript:alert"));
        assert!(
            blocks
                .last()
                .unwrap()
                .spans
                .iter()
                .any(|s| s.link.is_some())
        );
        assert_eq!(parse("**literal**", false)[0].text, "**literal**");
    }
    #[test]
    fn a_reply_card_line_is_plain_text() {
        assert_eq!(
            reply_first_line("\n## **Done**:   all  `three` workers\nsecond line").as_deref(),
            Some("Done: all three workers")
        );
        assert_eq!(
            reply_first_line("- first item\n- second").as_deref(),
            Some("\u{2022} first item")
        );
        assert_eq!(
            reply_first_line("```bash\nls\n```\nafter").as_deref(),
            Some("ls")
        );
        assert_eq!(reply_first_line("  \n\t"), None);
        assert_eq!(
            reply_first_line(&"x".repeat(500)).unwrap().chars().count(),
            200
        );
    }
}
