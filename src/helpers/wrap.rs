/// Word-wrap `text` to at most `width` visible characters per line.
pub fn wrap_text(text: &str, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        if current.chars().count() + word.chars().count() + 1 > width && !current.is_empty() {
            out.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

/// Truncate `s` to at most `max` chars, appending `…` if truncated.
pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let cut: String = s.chars().take(max - 1).collect();
        format!("{cut}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap_splits_at_width() {
        let lines = wrap_text("hello world foo bar", 10);
        for l in &lines {
            assert!(l.chars().count() <= 10, "line too long: {l:?}");
        }
    }

    #[test]
    fn truncate_short_passthrough() {
        assert_eq!(truncate("hi", 10), "hi");
    }

    #[test]
    fn truncate_adds_ellipsis() {
        let s = truncate("hello world", 8);
        assert!(s.ends_with('…'));
        assert_eq!(s.chars().count(), 8);
    }
}
