//! Canonical inventory and prediction categories. User rule names are independent labels.

pub const CATEGORY_IDS: [&str; 7] = [
    "Documents", "Images", "Videos", "Audio", "Archives", "Code", "Other",
];

/// Exact wire-value validation; a category must never double as an arbitrary path.
pub fn canonical_category(value: &str) -> Option<&'static str> {
    CATEGORY_IDS.into_iter().find(|category| *category == value)
}

pub fn is_valid_category(value: &str) -> bool {
    canonical_category(value).is_some()
}

/// The one extension table used by inventory scans and classification.
pub fn extension_to_category(extension: &str) -> Option<&'static str> {
    let extension = extension.trim_start_matches('.').to_ascii_lowercase();
    match extension.as_str() {
        "pdf" | "doc" | "docx" | "txt" | "md" | "xls" | "xlsx" | "ppt" | "pptx"
        | "csv" | "rtf" | "odt" | "ods" | "odp" => Some("Documents"),
        "jpg" | "jpeg" | "png" | "gif" | "webp" | "svg" | "heic" | "heif" | "bmp"
        | "ico" => Some("Images"),
        "mp4" | "mkv" | "avi" | "mov" | "webm" | "wmv" | "flv" => Some("Videos"),
        "mp3" | "wav" | "flac" | "aac" | "ogg" | "m4a" | "wma" => Some("Audio"),
        "zip" | "rar" | "7z" | "tar" | "gz" | "bz2" | "xz" | "iso" | "dmg"
        | "exe" | "msi" | "msix" | "appx" | "deb" | "rpm" | "apk" | "appimage" => {
            Some("Archives")
        }
        "rs" | "ts" | "tsx" | "js" | "jsx" | "py" | "java" | "c" | "cpp" | "h"
        | "hpp" | "go" | "rb" | "cs" | "swift" | "kt" | "scala" | "php" | "pl"
        | "sh" | "bash" | "css" | "scss" | "less" | "html" | "htm" | "json"
        | "xml" | "yaml" | "yml" | "toml" | "sql" | "vue" | "svelte" | "astro" => {
            Some("Code")
        }
        _ => None,
    }
}

pub fn category_for_extension(extension: &str) -> &'static str {
    extension_to_category(extension).unwrap_or("Other")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn categories_are_labels_and_never_paths() {
        for category in CATEGORY_IDS {
            assert!(is_valid_category(category));
        }
        for value in ["../Images", "Images/../Code", "C:\\Images", "Music", "Others", "images", ""] {
            assert!(!is_valid_category(value));
        }
    }

    #[test]
    fn extension_table_covers_previously_divergent_scanner_entries() {
        for (extension, expected) in [
            (".RTF", "Documents"), ("ods", "Documents"), ("heif", "Images"),
            ("ICO", "Images"), ("flv", "Videos"), ("wma", "Audio"),
            ("xz", "Archives"), ("msi", "Archives"), ("jsx", "Code"),
            ("yaml", "Code"), ("", "Other"), ("unknown", "Other"),
        ] {
            assert_eq!(category_for_extension(extension), expected);
        }
    }
}
