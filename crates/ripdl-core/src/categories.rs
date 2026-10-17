use std::path::Path;

/// Broad file-type buckets used for auto-categorization (IDM-style folders).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Video,
    Audio,
    Image,
    Document,
    Archive,
    Program,
    Other,
}

impl Category {
    /// Subdirectory name under the user's download root, when applicable.
    pub fn subdir(&self) -> Option<&'static str> {
        match self {
            Category::Video => Some("Videos"),
            Category::Audio => Some("Music"),
            Category::Image => Some("Images"),
            Category::Document => Some("Documents"),
            Category::Archive => Some("Archives"),
            Category::Program => Some("Programs"),
            Category::Other => None,
        }
    }
}

pub fn categorize(filename: &str) -> Category {
    let ext = Path::new(filename)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    match ext.as_str() {
        "mp4" | "mkv" | "avi" | "mov" | "webm" | "flv" | "wmv" | "m4v" | "mpg" | "mpeg" => {
            Category::Video
        }
        "mp3" | "flac" | "wav" | "aac" | "ogg" | "m4a" | "opus" | "wma" => Category::Audio,
        "jpg" | "jpeg" | "png" | "gif" | "webp" | "bmp" | "svg" | "ico" => Category::Image,
        "pdf" | "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" | "txt" | "epub" | "mobi" => {
            Category::Document
        }
        "zip" | "rar" | "7z" | "tar" | "gz" | "xz" | "bz2" | "zst" | "iso" => Category::Archive,
        "exe" | "msi" | "apk" | "dmg" | "deb" | "rpm" | "appimage" | "appx" => Category::Program,
        _ => Category::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buckets_common_types() {
        assert_eq!(categorize("movie.mkv"), Category::Video);
        assert_eq!(categorize("SONG.MP3"), Category::Audio);
        assert_eq!(categorize("setup.exe"), Category::Program);
        assert_eq!(categorize("noext"), Category::Other);
    }
}
