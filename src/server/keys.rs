/// Normalize an object key from a path capture.
/// Trims leading `/`, rejects empty keys and `.` / `..` path segments.
pub fn normalize_object_key(raw: &str) -> Result<String, &'static str> {
    let key = raw.trim().trim_start_matches('/');
    if key.is_empty() {
        return Err("object key must not be empty");
    }
    for segment in key.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." {
            return Err("object key contains invalid path segment");
        }
    }
    Ok(key.to_string())
}

/// Normalize a folder prefix: no leading `/`, must end with `/`, valid segments.
pub fn normalize_folder_prefix(raw: &str) -> Result<String, &'static str> {
    let key = raw.trim().trim_start_matches('/');
    if key.is_empty() || key == "/" {
        return Err("folder prefix must not be empty");
    }
    let key = key.trim_end_matches('/');
    if key.is_empty() {
        return Err("folder prefix must not be empty");
    }
    for segment in key.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." {
            return Err("folder prefix contains invalid path segment");
        }
    }
    Ok(format!("{key}/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trims_leading_slash() {
        assert_eq!(normalize_object_key("/photos/a.jpg").unwrap(), "photos/a.jpg");
    }

    #[test]
    fn rejects_dot_dot() {
        assert!(normalize_object_key("a/../b").is_err());
        assert!(normalize_object_key("../b").is_err());
    }

    #[test]
    fn rejects_empty() {
        assert!(normalize_object_key("").is_err());
        assert!(normalize_object_key("/").is_err());
    }

    #[test]
    fn accepts_nested() {
        assert_eq!(
            normalize_object_key("photos/2024/cat.jpg").unwrap(),
            "photos/2024/cat.jpg"
        );
    }

    #[test]
    fn folder_prefix_adds_slash() {
        assert_eq!(
            normalize_folder_prefix("photos/vacation").unwrap(),
            "photos/vacation/"
        );
        assert_eq!(
            normalize_folder_prefix("/photos/vacation/").unwrap(),
            "photos/vacation/"
        );
    }

    #[test]
    fn folder_prefix_rejects_dot_dot() {
        assert!(normalize_folder_prefix("a/../b").is_err());
    }
}
