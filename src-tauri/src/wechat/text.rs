//! Pure WeChat outbound text/markdown processing.
//!
//! Extracted from `executor.rs`, which mixes effect execution (stdin, iLink
//! HTTP, circuit breaking) with this pure string layer. Everything here takes
//! text and returns text — no I/O, no `StdinManager`, no API client — so it
//! lives and is tested on its own.

/// WeChat rejects single text messages above this character count, so outbound
/// text is split into chunks no longer than this.
const MAX_WECHAT_TEXT_CHARS: usize = 3_800;

// ===== Outbound text splitting =====

fn split_wechat_text_chunks(text: &str) -> Vec<String> {
    if text.chars().count() <= MAX_WECHAT_TEXT_CHARS {
        return vec![text.to_string()];
    }

    let mut chunks = Vec::new();
    let mut remaining = text;
    while remaining.chars().count() > MAX_WECHAT_TEXT_CHARS {
        let hard_split = byte_index_after_chars(remaining, MAX_WECHAT_TEXT_CHARS);
        let split_at = preferred_split_boundary(remaining, hard_split).unwrap_or(hard_split);
        let (chunk, rest) = remaining.split_at(split_at);
        chunks.push(chunk.to_string());
        remaining = rest;
    }
    if !remaining.is_empty() {
        chunks.push(remaining.to_string());
    }
    chunks
}

pub(crate) fn split_wechat_text_messages_with_preferences(
    text: &str,
    split_line_breaks: bool,
) -> Vec<String> {
    if !split_line_breaks {
        return split_wechat_text_chunks(text)
            .into_iter()
            .filter(|chunk| !chunk.trim().is_empty())
            .collect();
    }

    split_wechat_line_units(text)
        .into_iter()
        .flat_map(|unit| split_wechat_text_chunks(&unit))
        .filter(|chunk| !chunk.trim().is_empty())
        .collect()
}

fn split_wechat_line_units(text: &str) -> Vec<String> {
    let mut units = Vec::new();
    let mut current = String::new();
    let mut in_code_fence = false;
    let mut in_table = false;

    for raw_line in text.split_inclusive('\n') {
        let line = raw_line.trim_end_matches(['\r', '\n']);
        let trimmed = line.trim();

        if trimmed.is_empty() && !in_code_fence {
            push_wechat_line_unit(&mut units, &mut current);
            in_table = false;
            continue;
        }

        let is_code_fence = trimmed.starts_with("```");
        if in_code_fence || is_code_fence {
            append_wechat_line(&mut current, line);
            if is_code_fence {
                in_code_fence = !in_code_fence;
            }
            continue;
        }

        let is_table_line = looks_like_markdown_table_line(trimmed);
        let is_continuation = !current.is_empty() && line.starts_with([' ', '\t']);
        if in_table && !is_table_line {
            push_wechat_line_unit(&mut units, &mut current);
        } else if !current.is_empty() && !is_continuation && !is_table_line {
            push_wechat_line_unit(&mut units, &mut current);
        }

        append_wechat_line(&mut current, line);
        in_table = is_table_line;
    }

    push_wechat_line_unit(&mut units, &mut current);
    units
}

fn push_wechat_line_unit(units: &mut Vec<String>, current: &mut String) {
    if current.trim().is_empty() {
        current.clear();
        return;
    }
    units.push(std::mem::take(current));
}

fn append_wechat_line(current: &mut String, line: &str) {
    if !current.is_empty() {
        current.push('\n');
    }
    current.push_str(line);
}

fn looks_like_markdown_table_line(line: &str) -> bool {
    line.starts_with('|') && line.ends_with('|') && line.matches('|').count() >= 2
}

fn preferred_split_boundary(text: &str, hard_split: usize) -> Option<usize> {
    let candidate = &text[..hard_split];
    candidate
        .char_indices()
        .rev()
        .find(|(_, ch)| *ch == '\n')
        .or_else(|| {
            candidate
                .char_indices()
                .rev()
                .find(|(_, ch)| ch.is_whitespace())
        })
        .map(|(index, ch)| index + ch.len_utf8())
        .filter(|index| *index > 0)
}

fn byte_index_after_chars(text: &str, max_chars: usize) -> usize {
    for (index, (byte_index, ch)) in text.char_indices().enumerate() {
        if index + 1 == max_chars {
            return byte_index + ch.len_utf8();
        }
    }
    text.len()
}

// ===== Markdown filtering =====

pub(crate) fn filter_wechat_markdown(text: &str) -> String {
    let mut filtered = String::with_capacity(text.len());
    let mut in_code_fence = false;

    for line in text.split_inclusive('\n') {
        let (body, line_break) = line
            .strip_suffix('\n')
            .map(|body| (body, "\n"))
            .unwrap_or((line, ""));

        if body.starts_with("```") {
            in_code_fence = !in_code_fence;
            filtered.push_str(line);
            continue;
        }
        if in_code_fence {
            filtered.push_str(line);
            continue;
        }

        filtered.push_str(&filter_wechat_inline_markdown(strip_wechat_line_markers(
            body,
        )));
        filtered.push_str(line_break);
    }

    filtered
}

fn strip_wechat_line_markers(line: &str) -> &str {
    if let Some(rest) = line.strip_prefix('>') {
        return rest.trim_start_matches([' ', '\t']);
    }

    let hash_count = line.bytes().take_while(|byte| *byte == b'#').count();
    if (5..=6).contains(&hash_count) && line.as_bytes().get(hash_count) == Some(&b' ') {
        return line[hash_count + 1..].trim_start_matches([' ', '\t']);
    }

    line
}

fn filter_wechat_inline_markdown(line: &str) -> String {
    let without_images = remove_markdown_images(line);
    let without_strikethrough = without_images.replace("~~", "");
    strip_cjk_emphasis(&without_strikethrough)
}

fn remove_markdown_images(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut remaining = text;

    while let Some(start) = remaining.find("![") {
        out.push_str(&remaining[..start]);
        let after_start = &remaining[start + 2..];
        let Some(label_end) = after_start.find("](") else {
            out.push_str(&remaining[start..]);
            return out;
        };
        let after_url_start = &after_start[label_end + 2..];
        let Some(url_end) = after_url_start.find(')') else {
            out.push_str(&remaining[start..]);
            return out;
        };
        remaining = &after_url_start[url_end + 1..];
    }

    out.push_str(remaining);
    out
}

// ===== CJK emphasis stripping =====

/// Strip Markdown emphasis markers (`*`, `_`, `***`, `___`) that wrap a CJK run.
///
/// WeChat renders these markers literally for CJK text instead of as emphasis,
/// so we drop them; ASCII emphasis is left untouched.
pub(crate) fn strip_cjk_emphasis(text: &str) -> String {
    let text = strip_cjk_wrapping_marker(text, "***");
    let text = strip_cjk_wrapping_marker(&text, "___");
    let text = strip_cjk_single_marker(&text, '*');
    strip_cjk_single_marker(&text, '_')
}

fn strip_cjk_wrapping_marker(text: &str, marker: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut remaining = text;

    while let Some(start) = remaining.find(marker) {
        out.push_str(&remaining[..start]);
        let content_start = start + marker.len();
        let Some(end) = remaining[content_start..].find(marker) else {
            out.push_str(&remaining[start..]);
            return out;
        };
        let content_end = content_start + end;
        let content = &remaining[content_start..content_end];
        if contains_cjk(content) {
            out.push_str(content);
        } else {
            out.push_str(marker);
            out.push_str(content);
            out.push_str(marker);
        }
        remaining = &remaining[content_end + marker.len()..];
    }

    out.push_str(remaining);
    out
}

fn strip_cjk_single_marker(text: &str, marker: char) -> String {
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0;

    while let Some(start) = find_single_marker(text, marker, cursor) {
        out.push_str(&text[cursor..start]);
        let content_start = start + marker.len_utf8();
        let Some(end) = find_single_marker(text, marker, content_start) else {
            out.push_str(&text[start..]);
            return out;
        };
        let content = &text[content_start..end];
        if contains_cjk(content) {
            out.push_str(content);
        } else {
            out.push(marker);
            out.push_str(content);
            out.push(marker);
        }
        cursor = end + marker.len_utf8();
    }

    out.push_str(&text[cursor..]);
    out
}

fn find_single_marker(text: &str, marker: char, start: usize) -> Option<usize> {
    text[start..].char_indices().find_map(|(offset, ch)| {
        let index = start + offset;
        if ch == marker && !has_adjacent_marker(text, index, marker) {
            Some(index)
        } else {
            None
        }
    })
}

fn has_adjacent_marker(text: &str, index: usize, marker: char) -> bool {
    let before = text[..index].chars().next_back();
    let after = text[index + marker.len_utf8()..].chars().next();
    before == Some(marker) || after == Some(marker)
}

fn contains_cjk(text: &str) -> bool {
    text.chars().any(|ch| {
        ('\u{2E80}'..='\u{9FFF}').contains(&ch)
            || ('\u{AC00}'..='\u{D7AF}').contains(&ch)
            || ('\u{F900}'..='\u{FAFF}').contains(&ch)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // 特征测试：断言的是这些函数「当前的真实输出」（用记录法录得），不是
    // 「理论上应该如何」。重构若改动任一输出，这里就变红。
    // 注意 "**你好**" → "**你好**"：双星号粗体不在此层剥除（由上游处理）。
    // 这是当前行为，特意冻住，免得重构时被「顺手修正」而悄悄改了行为。

    #[test]
    fn strip_cjk_emphasis_drops_markers_around_cjk_keeps_ascii() {
        assert_eq!(strip_cjk_emphasis("***你好***"), "你好");
        assert_eq!(strip_cjk_emphasis("*你好*"), "你好");
        assert_eq!(strip_cjk_emphasis("___你好___"), "你好");
        assert_eq!(strip_cjk_emphasis("_你好_"), "你好");
        assert_eq!(strip_cjk_emphasis("前*缀*后"), "前缀后");
        // ASCII 强调保留；双星号粗体当前不剥：
        assert_eq!(strip_cjk_emphasis("**hello**"), "**hello**");
        assert_eq!(strip_cjk_emphasis("**你好**"), "**你好**");
        // 混合：ASCII 部分保留，CJK 部分剥除
        assert_eq!(strip_cjk_emphasis("*hi* 还有 *你*"), "*hi* 还有 你");
        assert_eq!(strip_cjk_emphasis(""), "");
    }

    #[test]
    fn contains_cjk_detects_han_and_hangul_not_latin() {
        assert!(contains_cjk("你好"));
        assert!(contains_cjk("日本語ABC"));
        assert!(contains_cjk("한국"));
        assert!(!contains_cjk("hello"));
        assert!(!contains_cjk("café"));
        assert!(!contains_cjk(""));
    }

    #[test]
    fn strip_cjk_wrapping_marker_only_unwraps_cjk() {
        assert_eq!(strip_cjk_wrapping_marker("***你***", "***"), "你");
        assert_eq!(strip_cjk_wrapping_marker("***hi***", "***"), "***hi***");
        // 第二个标记缺失 → 原样返回
        assert_eq!(strip_cjk_wrapping_marker("***你", "***"), "***你");
    }

    #[test]
    fn strip_cjk_single_marker_unwraps_cjk_and_skips_adjacent_doubles() {
        assert_eq!(strip_cjk_single_marker("*你*", '*'), "你");
        assert_eq!(strip_cjk_single_marker("*hi*", '*'), "*hi*");
        // 相邻双星号不被单标记逻辑吃掉（has_adjacent_marker 的职责）。
        // "*你**好*" 是判别输入：has_adjacent_marker 一旦失效，中间的 ** 会被
        // 误当单标记吃掉、输出变成 "你好"；保留 ** 才证明它在工作。
        assert_eq!(strip_cjk_single_marker("**你**", '*'), "**你**");
        assert_eq!(strip_cjk_single_marker("a*b*c", '*'), "a*b*c");
        assert_eq!(strip_cjk_single_marker("*你**好*", '*'), "你**好");
    }

    #[test]
    fn split_wechat_text_chunks_keeps_unicode_characters_intact_without_boundaries() {
        let text = format!("{}{}", "好".repeat(MAX_WECHAT_TEXT_CHARS), "界");

        let chunks = split_wechat_text_chunks(&text);

        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].chars().count(), MAX_WECHAT_TEXT_CHARS);
        assert_eq!(chunks[0], "好".repeat(MAX_WECHAT_TEXT_CHARS));
        assert_eq!(chunks[1], "界");
    }
}
