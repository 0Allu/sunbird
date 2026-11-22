//! IDA-style byte patterns ("48 8B ?? 05 ? E8"), searched with memchr on the first fixed byte.

pub struct Pattern {
    bytes: Vec<Option<u8>>,
    anchor: usize, // index of the first fixed byte
}

impl Pattern {
    pub fn parse(s: &str) -> Result<Pattern, String> {
        let bytes = s.split_whitespace()
            .map(|t| {
                if t.chars().all(|c| c == '?') { Ok(None) }
                else { u8::from_str_radix(t, 16).map(Some).map_err(|_| format!("bad byte `{t}`")) }
            })
            .collect::<Result<Vec<_>, _>>()?;
        let anchor = bytes.iter().position(|b| b.is_some()).ok_or("pattern has no fixed bytes")?;
        Ok(Pattern { bytes, anchor })
    }

    fn at(&self, hay: &[u8], start: usize) -> bool {
        hay.len() >= start + self.bytes.len()
            && self.bytes.iter().zip(&hay[start..]).all(|(p, h)| p.map_or(true, |p| p == *h))
    }

    /// Offsets of every match in `hay`.
    pub fn find_all<'a>(&'a self, hay: &'a [u8]) -> impl Iterator<Item = usize> + 'a {
        let first = self.bytes[self.anchor].unwrap();
        memchr::memchr_iter(first, hay)
            .filter_map(move |p| p.checked_sub(self.anchor))
            .filter(move |&s| self.at(hay, s))
    }
}

#[test]
fn scan_works() {
    let hay = [0x90, 0x48, 0x8b, 0x05, 0x11, 0x22, 0xe8, 0x48, 0x8b, 0x06];
    let p = Pattern::parse("48 8B ?? ?? 22").unwrap();
    assert_eq!(p.find_all(&hay).collect::<Vec<_>>(), vec![1]);
    let p = Pattern::parse("?? 8b").unwrap();
    assert_eq!(p.find_all(&hay).collect::<Vec<_>>(), vec![1, 7]);
    assert!(Pattern::parse("?? ??").is_err());
    assert!(Pattern::parse("4G").is_err());
}
