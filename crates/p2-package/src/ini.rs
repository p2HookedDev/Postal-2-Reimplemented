use std::collections::HashMap;
use std::fmt::Write as _;

#[derive(Debug, Clone, Default)]
pub struct Ini {
    sections: HashMap<String, Vec<(String, String)>>,
}

impl Ini {
    pub fn parse(text: &str) -> Self {
        let mut sections: HashMap<String, Vec<(String, String)>> = HashMap::new();
        let mut current: Option<String> = None;
        for line in text.lines().map(str::trim) {
            if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                let name = name.trim().to_ascii_lowercase();
                sections.entry(name.clone()).or_default();
                current = Some(name);
            } else if let (Some(section), Some((key, value))) = (&current, line.split_once('='))
                && !line.starts_with(';')
                && let Some(entries) = sections.get_mut(section)
            {
                entries.push((key.trim().to_owned(), value.trim().to_owned()));
            }
        }
        Self { sections }
    }

    pub fn read(path: impl AsRef<std::path::Path>) -> std::io::Result<Self> {
        Ok(Self::parse(&String::from_utf8_lossy(&std::fs::read(path)?)))
    }

    pub fn get(&self, section: &str, key: &str) -> Option<&str> {
        self.section(section).iter().find(|(k, _)| k.eq_ignore_ascii_case(key)).map(|(_, v)| v.as_str())
    }

    /// [`Ini::parse`] of a file's bytes: UTF-16 after its byte order mark (the install's `Subtitles.ini` is big-endian),
    /// else UTF-8, with or without its mark.
    pub fn from_bytes(bytes: &[u8]) -> Self {
        let utf16 = |unit: fn([u8; 2]) -> u16| {
            let units: Vec<u16> = bytes[2..].as_chunks::<2>().0.iter().map(|pair| unit(*pair)).collect();
            String::from_utf16_lossy(&units)
        };
        match bytes {
            [0xFF, 0xFE, ..] => Self::parse(&utf16(u16::from_le_bytes)),
            [0xFE, 0xFF, ..] => Self::parse(&utf16(u16::from_be_bytes)),
            [0xEF, 0xBB, 0xBF, rest @ ..] => Self::parse(&String::from_utf8_lossy(rest)),
            _ => Self::parse(&String::from_utf8_lossy(bytes)),
        }
    }

    pub fn get_all<'a>(&'a self, section: &str, key: &str) -> impl Iterator<Item = &'a str> + use<'a> {
        let key = key.to_ascii_lowercase();
        self.sections
            .get(&section.to_ascii_lowercase())
            .into_iter()
            .flatten()
            .filter(move |(k, _)| k.eq_ignore_ascii_case(&key))
            .map(|(_, v)| v.as_str())
    }

    pub fn section(&self, section: &str) -> &[(String, String)] {
        self.sections.get(&section.to_ascii_lowercase()).map_or(&[], Vec::as_slice)
    }

    pub fn set(&mut self, section: &str, key: &str, value: &str) {
        let entries = self.sections.entry(section.to_ascii_lowercase()).or_default();
        let mut seen = false;
        entries.retain_mut(|(k, v)| {
            if !k.eq_ignore_ascii_case(key) {
                return true;
            }
            value.clone_into(v);
            !std::mem::replace(&mut seen, true)
        });
        if !seen {
            entries.push((key.to_owned(), value.to_owned()));
        }
    }

    pub fn set_all(&mut self, section: &str, key: &str, values: &[String]) {
        let entries = self.sections.entry(section.to_ascii_lowercase()).or_default();
        let at = entries.iter().position(|(k, _)| k.eq_ignore_ascii_case(key)).unwrap_or(entries.len());
        entries.retain(|(k, _)| !k.eq_ignore_ascii_case(key));
        let at = at.min(entries.len());
        entries.splice(at..at, values.iter().map(|value| (key.to_owned(), value.clone())));
    }

    pub fn to_text(&self) -> String {
        let mut names: Vec<&String> = self.sections.keys().collect();
        names.sort();
        let mut text = String::new();
        for name in names {
            let _ = writeln!(text, "[{name}]");
            for (key, value) in &self.sections[name] {
                let _ = writeln!(text, "{key}={value}");
            }
            text.push('\n');
        }
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sections_keys_and_repeats() {
        let ini = Ini::parse("top=ignored\n[Engine.GameInfo]\nGoreLevel=0\n; comment=1\n[engine.gameinfo]\n\
                              Paths = a \nPaths=b\nbroken line\n[Empty]\n");
        assert_eq!(ini.get("ENGINE.GAMEINFO", "gorelevel"), Some("0"));
        assert_eq!(ini.get_all("Engine.GameInfo", "Paths").collect::<Vec<_>>(), ["a", "b"]);
        assert_eq!(ini.get("Engine.GameInfo", "comment"), None);
        assert_eq!(ini.get("Nope", "Paths"), None);
        assert!(ini.section("Empty").is_empty());
    }

    #[test]
    fn set_replaces_and_round_trips() {
        let mut ini = Ini::parse("[A]\nx=1\ny=2\nx=3\n");
        ini.set("a", "X", "9");
        ini.set("B", "z", "True");
        assert_eq!(ini.get_all("A", "x").collect::<Vec<_>>(), ["9"]);
        let again = Ini::parse(&ini.to_text());
        assert_eq!((again.get("A", "y"), again.get("b", "Z")), (Some("2"), Some("True")));
    }

    #[test]
    fn set_all_replaces_a_repeated_key_in_place() {
        let mut ini = Ini::parse("[A]\nx=1\ny=2\nx=3\n");
        ini.set_all("A", "X", &["a".to_owned(), "b".to_owned(), "c".to_owned()]);
        let keys: Vec<String> = ini.section("A").iter().map(|(k, v)| format!("{k}={v}")).collect();
        assert_eq!(keys, ["X=a", "X=b", "X=c", "y=2"]);
        ini.set_all("B", "z", &["1".to_owned()]);
        assert_eq!(ini.get_all("b", "Z").collect::<Vec<_>>(), ["1"]);
        ini.set_all("A", "x", &[]);
        assert_eq!(ini.get("A", "x"), None);
    }

    #[test]
    fn from_bytes_decodes_by_byte_order_mark() {
        let text = "[A]\r\nx=\u{427}\u{443}\r\n";
        let utf16 = |mark: [u8; 2], unit: fn(u16) -> [u8; 2]| -> Vec<u8> { mark.into_iter().chain(text.encode_utf16().flat_map(unit)).collect() };
        let marked = [&[0xEFu8, 0xBB, 0xBF][..], text.as_bytes()].concat();
        for bytes in [utf16([0xFF, 0xFE], u16::to_le_bytes), utf16([0xFE, 0xFF], u16::to_be_bytes), marked, text.as_bytes().to_vec()] {
            assert_eq!(Ini::from_bytes(&bytes).get("A", "x"), Some("\u{427}\u{443}"));
        }
        assert_eq!(Ini::from_bytes(&[]).get("A", "x"), None);
    }
}
