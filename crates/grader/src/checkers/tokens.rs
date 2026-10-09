pub fn tokens(bytes: &[u8], unicode: bool) -> Vec<&[u8]> {
    if unicode && let Ok(text) = std::str::from_utf8(bytes) {
        return text.split_whitespace().map(str::as_bytes).collect();
    }
    bytes.split(|b| b.is_ascii_whitespace()).filter(|t| !t.is_empty()).collect()
}

#[cfg(test)]
mod tests {
    use super::tokens;

    #[test]
    fn ascii_whitespace_splits_and_collapses() {
        assert_eq!(tokens(b"  1\t2\r\n\n3 ", false), [b"1" as &[u8], b"2", b"3"]);
        assert!(tokens(b" \n\t", false).is_empty());
    }

    #[test]
    fn unicode_mode_splits_on_unicode_spaces_only_when_asked() {
        let text = "a\u{3000}b".as_bytes(); // ideographic space
        assert_eq!(tokens(text, true).len(), 2);
        assert_eq!(tokens(text, false).len(), 1);
    }
}
