//! Text encodings and line endings: detection on open, conversion on save.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum LineEnding {
    #[default]
    Unix,
    Windows,
    Mac,
}

impl LineEnding {
    pub const ALL: [LineEnding; 3] = [LineEnding::Unix, LineEnding::Windows, LineEnding::Mac];

    pub fn name(self) -> &'static str {
        match self {
            LineEnding::Unix => "Unix",
            LineEnding::Windows => "Windows",
            LineEnding::Mac => "Mac OS 9",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            LineEnding::Unix => "\n",
            LineEnding::Windows => "\r\n",
            LineEnding::Mac => "\r",
        }
    }

    /// The first line ending found in `text`.
    pub fn detect(text: &str) -> Self {
        match text.find(['\r', '\n']) {
            Some(i) if text[i..].starts_with("\r\n") => LineEnding::Windows,
            Some(i) if text[i..].starts_with('\r') => LineEnding::Mac,
            _ => LineEnding::Unix,
        }
    }
}

/// Encodings offered in the UI, by Sublime's names.
pub const ENCODINGS: &[&str] = &[
    "UTF-8",
    "UTF-8 with BOM",
    "UTF-16 LE",
    "UTF-16 BE",
    "Greek (Windows 1253)",
    "Greek (ISO 8859-7)",
    "Western (Windows 1252)",
    "Western (ISO 8859-1)",
    "Cyrillic (Windows 1251)",
    "Central European (Windows 1250)",
];

fn codec(name: &str) -> &'static encoding_rs::Encoding {
    use encoding_rs::*;
    match name {
        "Greek (Windows 1253)" => WINDOWS_1253,
        "Greek (ISO 8859-7)" => ISO_8859_7,
        "Western (Windows 1252)" | "Western (ISO 8859-1)" => WINDOWS_1252,
        "Cyrillic (Windows 1251)" => WINDOWS_1251,
        "Central European (Windows 1250)" => WINDOWS_1250,
        "UTF-16 LE" => UTF_16LE,
        "UTF-16 BE" => UTF_16BE,
        _ => UTF_8,
    }
}

/// Guess a legacy 8-bit encoding: runs of 3+ high bytes are typical of Greek, rare in Western text.
fn guess_legacy(bytes: &[u8]) -> &'static str {
    let (mut run, mut greek_runs, mut high) = (0, 0, 0);
    for &b in bytes {
        if b >= 0xC1 && b != 0xD2 && b <= 0xFE {
            high += 1;
            run += 1;
            if run == 3 {
                greek_runs += 1;
            }
        } else {
            run = 0;
        }
    }
    if greek_runs > 0 && greek_runs * 20 >= high / 10 {
        "Greek (Windows 1253)"
    } else {
        "Western (Windows 1252)"
    }
}

/// Decode file bytes. `forced` picks the encoding; otherwise BOM, UTF-8 validity and a heuristic decide.
pub fn decode(bytes: &[u8], forced: Option<&str>) -> (String, String) {
    let name = match forced {
        Some(n) => n.to_string(),
        None if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) => "UTF-8 with BOM".into(),
        None if bytes.starts_with(&[0xFF, 0xFE]) => "UTF-16 LE".into(),
        None if bytes.starts_with(&[0xFE, 0xFF]) => "UTF-16 BE".into(),
        None if std::str::from_utf8(bytes).is_ok() => "UTF-8".into(),
        None => guess_legacy(bytes).into(),
    };
    let body = match name.as_str() {
        "UTF-8 with BOM" => bytes.strip_prefix(&[0xEF, 0xBB, 0xBF][..]).unwrap_or(bytes),
        "UTF-16 LE" => bytes.strip_prefix(&[0xFF, 0xFE][..]).unwrap_or(bytes),
        "UTF-16 BE" => bytes.strip_prefix(&[0xFE, 0xFF][..]).unwrap_or(bytes),
        _ => bytes,
    };
    let (text, _) = codec(&name).decode_without_bom_handling(body);
    (text.into_owned(), name)
}

pub fn encode(text: &str, name: &str) -> Vec<u8> {
    match name {
        "UTF-8" => text.as_bytes().to_vec(),
        "UTF-8 with BOM" => [&[0xEF, 0xBB, 0xBF][..], text.as_bytes()].concat(),
        // encoding_rs only decodes UTF-16, so encode it by hand.
        "UTF-16 LE" => [0xFF, 0xFE].into_iter().chain(text.encode_utf16().flat_map(|u| u.to_le_bytes())).collect(),
        "UTF-16 BE" => [0xFE, 0xFF].into_iter().chain(text.encode_utf16().flat_map(|u| u.to_be_bytes())).collect(),
        _ => codec(name).encode(text).0.into_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_greek_legacy() {
        let bytes = encode("Καλημέρα κόσμε, αυτό είναι ελληνικό κείμενο.", "Greek (Windows 1253)");
        let (text, enc) = decode(&bytes, None);
        assert_eq!(enc, "Greek (Windows 1253)");
        assert!(text.starts_with("Καλημέρα"));
    }

    #[test]
    fn detects_western_and_utf() {
        let (_, enc) = decode(&encode("café naïve résumé", "Western (Windows 1252)"), None);
        assert_eq!(enc, "Western (Windows 1252)");
        assert_eq!(decode("γεια".as_bytes(), None).1, "UTF-8");
        let b = encode("γεια", "UTF-16 LE");
        assert_eq!(decode(&b, None), ("γεια".to_string(), "UTF-16 LE".to_string()));
        let b = encode("x", "UTF-8 with BOM");
        assert_eq!(decode(&b, None), ("x".to_string(), "UTF-8 with BOM".to_string()));
    }

    #[test]
    fn line_endings() {
        assert_eq!(LineEnding::detect("a\r\nb"), LineEnding::Windows);
        assert_eq!(LineEnding::detect("a\nb"), LineEnding::Unix);
        assert_eq!(LineEnding::detect("a\rb"), LineEnding::Mac);
    }
}
