//! Editor transformations use character cursors at the UI boundary. The text on
//! disk is never normalized: only newly inserted text adopts the existing EOL.
use std::ops::Range;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct TextStyle {
    pub indent: String,
    pub newline: &'static str,
}

impl TextStyle {
    pub fn detect(text: &str) -> Self {
        let mut tabs = 0;
        let mut widths = Vec::new();
        for line in text.lines() {
            if line.trim().is_empty() {
                continue;
            }
            if line.starts_with('\t') {
                tabs += 1;
            }
            let spaces = line.bytes().take_while(|b| *b == b' ').count();
            if spaces > 0 {
                widths.push(spaces);
            }
        }
        let indent = if tabs > widths.len() {
            "\t"
        } else if widths.iter().any(|width| width % 4 == 2) {
            "  "
        } else {
            "    "
        };
        let newline = text.find('\n').map_or("\n", |index| {
            if index > 0 && text.as_bytes()[index - 1] == b'\r' {
                "\r\n"
            } else {
                "\n"
            }
        });
        Self {
            indent: indent.into(),
            newline,
        }
    }

    pub fn insertion(&self, text: &str) -> String {
        text.replace("\r\n", "\n")
            .replace('\r', "\n")
            .replace('\n', self.newline)
    }
}

pub(super) fn byte_offset(text: &str, chars: usize) -> usize {
    text.char_indices()
        .nth(chars)
        .map_or(text.len(), |(offset, _)| offset)
}

pub(super) fn char_offset(text: &str, bytes: usize) -> usize {
    text[..bytes.min(text.len())].chars().count()
}

pub(crate) fn byte_range(text: &str, chars: Range<usize>) -> Range<usize> {
    byte_offset(text, chars.start)..byte_offset(text, chars.end)
}

pub(super) fn replace(text: &mut String, chars: Range<usize>, replacement: &str) -> Range<usize> {
    let bytes = byte_range(text, chars.clone());
    text.replace_range(bytes, replacement);
    let end = chars.start + replacement.chars().count();
    end..end
}

/// A CRLF is one editing unit, including cursors placed between its two chars.
pub(super) fn delete_crlf(
    text: &mut String,
    selection: Range<usize>,
    backwards: bool,
) -> Option<Range<usize>> {
    let mut bytes = byte_range(text, selection.clone());
    if selection.is_empty() {
        let position = bytes.start;
        if position > 0
            && text.as_bytes().get(position - 1) == Some(&b'\r')
            && text.as_bytes().get(position) == Some(&b'\n')
        {
            bytes = position - 1..position + 1;
        } else if backwards && position >= 2 && &text.as_bytes()[position - 2..position] == b"\r\n"
        {
            bytes = position - 2..position;
        } else if !backwards
            && text.as_bytes().get(position..position + 2) == Some(b"\r\n".as_slice())
        {
            bytes = position..position + 2;
        } else {
            return None;
        }
    } else {
        if bytes.start > 0
            && text.as_bytes()[bytes.start - 1] == b'\r'
            && text.as_bytes().get(bytes.start) == Some(&b'\n')
        {
            bytes.start -= 1;
        }
        if bytes.end > 0
            && text.as_bytes()[bytes.end - 1] == b'\r'
            && text.as_bytes().get(bytes.end) == Some(&b'\n')
        {
            bytes.end += 1;
        }
    }
    let cursor = char_offset(text, bytes.start);
    text.replace_range(bytes, "");
    Some(cursor..cursor)
}

fn line_bounds(text: &str, position: usize) -> Range<usize> {
    let start = text[..position].rfind('\n').map_or(0, |index| index + 1);
    let end = text[position..]
        .find('\n')
        .map_or(text.len(), |index| position + index);
    start..end
}

fn indent_of(line: &str) -> &str {
    &line[..line
        .bytes()
        .take_while(|b| matches!(b, b' ' | b'\t'))
        .count()]
}

#[derive(Debug)]
struct Signature {
    line: Range<usize>,
    name: Range<usize>,
    open: usize,
    close: usize,
    parameters: Vec<Range<usize>>,
    returns: Option<Range<usize>>,
}

fn trim_range(text: &str, mut range: Range<usize>) -> Range<usize> {
    while range.start < range.end && text.as_bytes()[range.start].is_ascii_whitespace() {
        range.start += 1;
    }
    while range.end > range.start && text.as_bytes()[range.end - 1].is_ascii_whitespace() {
        range.end -= 1;
    }
    range
}

fn signature(text: &str, byte: usize) -> Option<Signature> {
    // Find the preceding declaration, then match its parameter list across
    // physical lines. Body positions never belong to the signature.
    let current_line = line_bounds(text, byte);
    let mut start = current_line.start;
    let mut line = loop {
        let candidate = line_bounds(text, start);
        let content = text[candidate.clone()].trim_start();
        if content.starts_with("def ") || content.starts_with("async def ") {
            break candidate;
        }
        if start == 0 {
            return None;
        }
        start = line_bounds(text, start - 1).start;
    };
    let content = &text[line.clone()];
    let leading = indent_of(content).len();
    let definition = content[leading..]
        .strip_prefix("async ")
        .map_or(leading, |_| leading + 6);
    if !content[definition..].starts_with("def ") {
        return None;
    }
    let name_start = line.start + definition + 4;
    let open = text[name_start..line.end].find('(')? + name_start;
    let name = trim_range(text, name_start..open);
    let mut depth = 0;
    let mut quote = None;
    let mut escape = false;
    let mut parameter_start = open + 1;
    let mut parameters = Vec::new();
    let mut close = None;
    let mut comment = false;
    for (relative, ch) in text[open + 1..].char_indices() {
        let index = open + 1 + relative;
        if comment {
            if ch == '\n' {
                comment = false;
            }
            continue;
        }
        if let Some(q) = quote {
            if escape {
                escape = false;
            } else if ch == '\\' {
                escape = true;
            } else if ch == q {
                quote = None;
            }
            continue;
        }
        match ch {
            '#' => comment = true,
            '\'' | '"' => quote = Some(ch),
            '(' | '[' | '{' => depth += 1,
            ')' if depth == 0 => {
                close = Some(index);
                break;
            }
            ')' | ']' | '}' => depth -= 1,
            ',' if depth == 0 => {
                let range = trim_range(text, parameter_start..index);
                if !range.is_empty() {
                    parameters.push(range);
                }
                parameter_start = index + 1;
            }
            _ => {}
        }
    }
    let close = close?;
    line.end = line_bounds(text, close).end;
    let last = trim_range(text, parameter_start..close);
    if !last.is_empty() {
        parameters.push(last);
    }
    let suffix = &text[close + 1..];
    let trimmed = suffix.trim_start();
    let returns = if trimmed.starts_with("->") {
        let start = close + 1 + suffix.len() - trimmed.len() + 2;
        let mut depth = 0;
        let mut quote = None;
        let mut escape = false;
        let mut end = line.end;
        for (relative, ch) in text[start..].char_indices() {
            if let Some(q) = quote {
                if escape {
                    escape = false;
                } else if ch == '\\' {
                    escape = true;
                } else if ch == q {
                    quote = None;
                }
                continue;
            }
            match ch {
                '\'' | '"' => quote = Some(ch),
                '(' | '[' | '{' => depth += 1,
                ')' | ']' | '}' => depth -= 1,
                ':' if depth == 0 => {
                    end = start + relative;
                    line.end = line_bounds(text, end).end;
                    break;
                }
                '#' if depth == 0 => {
                    end = start + relative;
                    break;
                }
                '\n' if depth == 0 => break,
                _ => {}
            }
        }
        Some(trim_range(text, start..end))
    } else {
        None
    };
    if byte > line.end {
        return None;
    }
    Some(Signature {
        line,
        name,
        open,
        close,
        parameters,
        returns,
    })
}

/// A declaration's opening parenthesis creates navigable parameter/return slots.
/// Only direct, non-static class methods receive `self` (class methods get `cls`).
pub(super) fn open_signature(text: &mut String, selection: Range<usize>) -> Option<Range<usize>> {
    if !selection.is_empty() {
        return None;
    }
    let byte = byte_offset(text, selection.start);
    let line = line_bounds(text, byte);
    let prefix = &text[line.start..byte];
    let indent = indent_of(prefix);
    let declaration = prefix
        .trim_start()
        .strip_prefix("async ")
        .unwrap_or(prefix.trim_start());
    let name = declaration.strip_prefix("def ")?;
    if name.is_empty() || !name.chars().all(|ch| ch == '_' || ch.is_alphanumeric()) {
        return None;
    }
    if !text[byte..line.end].trim().is_empty() {
        return None;
    }
    let mut is_class = false;
    let mut is_static = false;
    let mut is_classmethod = false;
    for previous in text[..line.start].lines().rev() {
        if previous.trim().is_empty() || previous.trim_start().starts_with('#') {
            continue;
        }
        let previous_indent = indent_of(previous);
        if previous_indent.len() == indent.len() && previous.trim_start().starts_with('@') {
            is_static |= previous.trim() == "@staticmethod";
            is_classmethod |= previous.trim() == "@classmethod";
            continue;
        }
        if previous_indent.len() < indent.len() {
            is_class = previous.trim_start().starts_with("class ");
            break;
        }
    }
    let receiver = if is_class && !is_static {
        if is_classmethod { "cls, " } else { "self, " }
    } else {
        ""
    };
    replace(text, selection.clone(), &format!("({receiver}) -> None:"));
    let cursor = selection.start + 1 + receiver.len();
    Some(cursor..cursor)
}

pub(super) fn tab(
    text: &mut String,
    selection: Range<usize>,
    reverse: bool,
    python: bool,
) -> Range<usize> {
    let bytes = byte_range(text, selection.clone());
    if python && let Some(signature) = signature(text, bytes.start) {
        if reverse {
            let previous = signature
                .parameters
                .iter()
                .rev()
                .find(|range| range.end < bytes.start)
                .unwrap_or(&signature.name);
            return char_offset(text, previous.start)..char_offset(text, previous.end);
        }
        if bytes.start < signature.open {
            if let Some(parameter) = signature
                .parameters
                .iter()
                .find(|range| !matches!(&text[(*range).clone()], "self" | "cls"))
            {
                return char_offset(text, parameter.start)..char_offset(text, parameter.end);
            }
            let cursor = char_offset(text, signature.close);
            return cursor..cursor;
        }
        if bytes.start <= signature.close {
            if let Some(returns) = signature.returns {
                if returns.is_empty() {
                    text.insert_str(returns.start, "None");
                    let start = char_offset(text, returns.start);
                    return start..start + 4;
                }
                return char_offset(text, returns.start)..char_offset(text, returns.end);
            }
            let insertion = signature.close + 1;
            text.insert_str(insertion, " -> None");
            let start = char_offset(text, insertion) + 4;
            return start..start + 4;
        }
        return enter(text, selection, true);
    }
    let style = TextStyle::detect(text);
    let first = line_bounds(text, bytes.start).start;
    let last = line_bounds(
        text,
        byte_offset(
            text,
            selection
                .end
                .saturating_sub(usize::from(!selection.is_empty())),
        ),
    )
    .end;
    if !reverse && !text[bytes.clone()].contains('\n') {
        let column = text[first..bytes.start].chars().count();
        let insertion = if style.indent == "\t" {
            "\t".into()
        } else {
            " ".repeat(style.indent.len() - column % style.indent.len())
        };
        return replace(text, selection, &insertion);
    }
    let mut result = String::new();
    let mut start_delta: isize = 0;
    let mut end_delta: isize = 0;
    let mut line_offset = char_offset(text, first);
    for line in text[first..last].split_inclusive('\n') {
        if reverse {
            let remove = if line.starts_with('\t') {
                1
            } else {
                line.bytes()
                    .take_while(|b| *b == b' ')
                    .count()
                    .min(style.indent.len())
            };
            result.push_str(&line[remove..]);
            start_delta -= selection.start.saturating_sub(line_offset).min(remove) as isize;
            end_delta -= selection.end.saturating_sub(line_offset).min(remove) as isize;
        } else {
            result.push_str(&style.indent);
            result.push_str(line);
            if selection.start >= line_offset {
                start_delta += style.indent.len() as isize;
            }
            if selection.end >= line_offset {
                end_delta += style.indent.len() as isize;
            }
        }
        line_offset += line.chars().count();
    }
    text.replace_range(first..last, &result);
    selection.start.saturating_add_signed(start_delta)
        ..selection.end.saturating_add_signed(end_delta)
}

pub(super) fn enter(text: &mut String, selection: Range<usize>, python: bool) -> Range<usize> {
    let byte = byte_offset(text, selection.start);
    let style = TextStyle::detect(text);
    if python && let Some(signature) = signature(text, byte) {
        let indent = format!(
            "{}{}",
            indent_of(&text[signature.line.clone()]),
            style.indent
        );
        let next = signature.line.end + usize::from(signature.line.end < text.len());
        if next < text.len() {
            let next_line = line_bounds(text, next);
            let content = &text[next_line.clone()];
            if !content.trim().is_empty() && indent_of(content).len() >= indent.len() {
                let cursor = char_offset(text, next_line.start + indent_of(content).len());
                return cursor..cursor;
            }
        }
        let insert_at = if text.as_bytes().get(signature.line.end.saturating_sub(1)) == Some(&b'\r')
        {
            signature.line.end - 1
        } else {
            signature.line.end
        };
        let insertion = format!("{}{indent}", style.newline);
        text.insert_str(insert_at, &insertion);
        let cursor = char_offset(text, insert_at + insertion.len());
        return cursor..cursor;
    }
    let bounds = line_bounds(text, byte);
    let prefix = &text[bounds.start..byte];
    let mut indent = indent_of(prefix).to_string();
    if python && prefix.trim_end().ends_with(':') {
        indent.push_str(&style.indent);
    }
    replace(text, selection, &format!("{}{indent}", style.newline))
}

pub(super) fn reorder_parameter(
    text: &mut String,
    selection: Range<usize>,
    right: bool,
) -> Option<Range<usize>> {
    if selection.is_empty() {
        return None;
    }
    let selected = byte_range(text, selection);
    let signature = signature(text, selected.start)?;
    let index = signature
        .parameters
        .iter()
        .position(|range| range.start <= selected.start && selected.end <= range.end)?;
    let other = if right {
        index.checked_add(1)?
    } else {
        index.checked_sub(1)?
    };
    let other_range = signature.parameters.get(other)?;
    // Separators and receiver slots cannot become positional parameters.
    let current = &text[signature.parameters[index].clone()];
    let adjacent = &text[other_range.clone()];
    let protected = |value: &str| {
        matches!(
            value.split([':', '=']).next().unwrap_or(value).trim(),
            "self" | "cls" | "/" | "*"
        ) || value.starts_with('*')
            || value.contains('#')
    };
    if protected(current) || protected(adjacent) {
        return None;
    }
    let current = current.to_owned();
    let adjacent = adjacent.to_owned();
    let current_range = signature.parameters[index].clone();
    let other_range = other_range.clone();
    let destination = if right {
        text.replace_range(other_range.clone(), &current);
        text.replace_range(current_range, &adjacent);
        other_range
            .start
            .saturating_add_signed(adjacent.len() as isize - current.len() as isize)
    } else {
        text.replace_range(current_range, &adjacent);
        text.replace_range(other_range.clone(), &current);
        other_range.start
    };
    let start = char_offset(text, destination);
    Some(start..start + current.chars().count())
}

pub(super) fn skip_closing_signature(text: &str, selection: Range<usize>) -> Option<Range<usize>> {
    if !selection.is_empty() {
        return None;
    }
    let byte = byte_offset(text, selection.start);
    let declaration = signature(text, byte)?;
    if declaration.close != byte {
        return None;
    }
    let next = selection.start + 1;
    Some(next..next)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn indent_selection_uses_unicode_boundaries_and_unindent_stays_in_its_line() {
        let mut unicode = "é".to_owned();
        tab(&mut unicode, 0..1, false, false);
        assert_eq!(unicode, "    ");
        let mut unicode = "é".to_owned();
        assert_eq!(tab(&mut unicode, 0..1, true, false), 0..1);
        assert_eq!(unicode, "é");
        let mut text = "a\n    b".to_owned();
        assert_eq!(tab(&mut text, 2..2, true, false), 2..2);
        assert_eq!(text, "a\nb");
        let mut text = "a\n    b\n    c".to_owned();
        assert_eq!(tab(&mut text, 2..10, true, false), 2..4);
        assert_eq!(text, "a\nb\nc");
    }
    #[test]
    fn indentation_and_newlines_follow_existing_source() {
        for (source, expected) in [
            ("if x:\n  y", "  "),
            ("if x:\r\n    y", "    "),
            ("if x:\n\ty", "\t"),
        ] {
            let style = TextStyle::detect(source);
            assert_eq!(style.indent, expected);
            let mut text = source.to_owned();
            let end = text.chars().count();
            let cursor = enter(&mut text, end..end, true);
            assert!(text.ends_with(&format!("{}{expected}", style.newline)));
            tab(&mut text, cursor, false, false);
            assert!(text.ends_with(&format!("{expected}{expected}")));
            assert_eq!(
                style.insertion("a\r\nb\nc"),
                format!("a{}b{}c", style.newline, style.newline)
            );
        }
    }
    #[test]
    fn class_method_slots_return_navigation_and_body_preserve_crlf() {
        let mut text = "class Example:\r\n  def charge".to_string();
        let end = text.chars().count();
        let cursor = open_signature(&mut text, end..end).unwrap();
        assert!(text.ends_with("def charge(self, ) -> None:"));
        let cursor = replace(&mut text, cursor, "amount: int");
        let returns = tab(&mut text, cursor, false, true);
        assert_eq!(&text[byte_range(&text, returns.clone())], "None");
        let previous = tab(&mut text, returns.clone(), true, true);
        assert_eq!(&text[byte_range(&text, previous)], "amount: int");
        enter(&mut text, returns, true);
        assert!(text.ends_with("\r\n    "));
        assert!(!text.replace("\r\n", "").contains('\n'));
    }
    #[test]
    fn self_is_not_added_to_free_static_or_nested_functions() {
        for prefix in [
            "def build",
            "class C:\n    @staticmethod\n    def build",
            "class C:\n    def parent(self):\n        def build",
        ] {
            let mut text = prefix.to_owned();
            let end = text.chars().count();
            open_signature(&mut text, end..end).unwrap();
            assert!(text.ends_with("build() -> None:"), "{text}");
        }
    }
    #[test]
    fn reorder_keeps_nested_annotation_defaults_and_unicode() {
        let mut text = "def test(élève: tuple[int, str], value: str = 'a,b') -> None:".to_string();
        let start = char_offset(&text, text.find("élève").unwrap());
        let selection = reorder_parameter(&mut text, start..start + 5, true).unwrap();
        assert_eq!(
            text,
            "def test(value: str = 'a,b', élève: tuple[int, str]) -> None:"
        );
        assert_eq!(
            &text[byte_range(&text, selection)],
            "élève: tuple[int, str]"
        );
    }
    #[test]
    fn backspace_and_delete_do_not_leave_half_a_crlf() {
        for (cursor, backwards) in [(3, true), (1, false), (2, true), (2, false)] {
            let mut source = "a\r\nb".to_owned();
            assert_eq!(
                delete_crlf(&mut source, cursor..cursor, backwards),
                Some(1..1)
            );
            assert_eq!(source, "ab");
        }
    }
    #[test]
    fn multiline_signature_navigation_reordering_and_body_keep_layout() {
        let mut text =
            "def fn(\r\n  first: int,\r\n  second: str,\r\n) -> bool:\r\n  return True\r\n"
                .to_owned();
        let start = text.find("first").unwrap();
        let moved = reorder_parameter(&mut text, start..start + 5, true).unwrap();
        assert!(text.contains("\r\n  second: str,\r\n  first: int,\r\n"));
        let returns = tab(&mut text, moved, false, true);
        assert_eq!(&text[byte_range(&text, returns.clone())], "bool");
        let body = enter(&mut text, returns, true);
        assert_eq!(&text[byte_offset(&text, body.start)..], "return True\r\n");
        assert_eq!(text.matches("return True").count(), 1);
    }
}
