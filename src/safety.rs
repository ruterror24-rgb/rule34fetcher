const SEGMENT_BLOCKED: &[&str] = &[
    "loli", "shota", "lolicon", "shotacon", "underage", "preteen", "toddler", "infant",
];

const EXACT_BLOCKED: &[&str] = &[
    "child", "children", "minor", "minors", "baby", "kid", "kids", "cub",
];

/// Возвращает запрещённый тег, если запрос или пост относится к несовершеннолетним.
pub fn blocked_tag(tags: &str) -> Option<&'static str> {
    for raw in tags.split_whitespace() {
        let raw = raw.trim().trim_matches(|c| c == '(' || c == ')');
        if raw.starts_with('-') || raw.is_empty() {
            continue;
        }
        let tag = raw.to_ascii_lowercase();
        if let Some(found) = lookup(&EXACT_BLOCKED, &tag).or_else(|| lookup(&SEGMENT_BLOCKED, &tag))
        {
            return Some(found);
        }
        for part in tag.split(|c: char| c == '_' || c == '-' || c == ':') {
            if let Some(found) = lookup(&SEGMENT_BLOCKED, part) {
                return Some(found);
            }
        }
    }
    None
}

fn lookup(list: &[&'static str], value: &str) -> Option<&'static str> {
    list.iter().copied().find(|item| *item == value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_minor_tag_segments() {
        assert_eq!(blocked_tag("solo loli"), Some("loli"));
        assert_eq!(blocked_tag("character_shota"), Some("shota"));
    }

    #[test]
    fn allows_ordinary_tags_and_exclusions() {
        assert_eq!(blocked_tag("1girl solo highres"), None);
        assert_eq!(blocked_tag("1girl -loli"), None);
        assert_eq!(blocked_tag("kid_icarus"), None);
        assert_eq!(blocked_tag("child"), Some("child"));
    }
}
