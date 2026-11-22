//! Token colors for JavaScript, for the code editor and the console.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
   Plain,
   Comment,
   String,
   Keyword,
   Number,
}

const KEYWORDS: &[&str] = &[
   "async", "await", "break", "case", "catch", "class", "const", "continue", "default", "delete",
   "do", "else", "export", "extends", "false", "finally", "for", "from", "function", "if",
   "import", "in", "instanceof", "let", "new", "null", "of", "return", "static", "super",
   "switch", "this", "throw", "true", "try", "typeof", "undefined", "var", "void", "while",
   "yield",
];

/// Splits `source` into colored runs that together are all of it.
// ponytail: no regex literals or template `${}` nesting; both read as strings
// or plain text, which only miscolors, never drops text.
#[must_use]
pub fn js(source: &str) -> Vec<(Kind, &str)> {
   let bytes = source.as_bytes();
   let len = bytes.len();
   let mut runs: Vec<(Kind, usize, usize)> = Vec::new();
   let mut at = 0;
   while at < len {
      let start = at;
      let byte = bytes[at];
      let next = bytes.get(at + 1).copied();
      let kind = match byte {
         b'/' if next == Some(b'/') => {
            at = source[at..].find('\n').map_or(len, |end| at + end);
            Kind::Comment
         },
         b'/' if next == Some(b'*') => {
            at = source[at + 2..].find("*/").map_or(len, |end| at + 2 + end + 2);
            Kind::Comment
         },
         b'"' | b'\'' | b'`' => {
            at += 1;
            while at < len {
               match bytes[at] {
                  b'\\' => at += 2,
                  quote if quote == byte => {
                     at += 1;
                     break;
                  },
                  b'\n' if byte != b'`' => break,
                  _ => at += 1,
               }
            }
            at = at.min(len);
            // An escape may have stepped into a multibyte character.
            while !source.is_char_boundary(at) {
               at += 1;
            }
            Kind::String
         },
         b'0'..=b'9' => {
            while at < len && (bytes[at].is_ascii_alphanumeric() || bytes[at] == b'.') {
               at += 1;
            }
            Kind::Number
         },
         _ if byte.is_ascii_alphabetic() || byte == b'_' || byte == b'$' => {
            while at < len
               && (bytes[at].is_ascii_alphanumeric() || bytes[at] == b'_' || bytes[at] == b'$')
            {
               at += 1;
            }
            if KEYWORDS.contains(&&source[start..at]) {
               Kind::Keyword
            } else {
               Kind::Plain
            }
         },
         _ => {
            at += 1;
            while at < len && !source.is_char_boundary(at) {
               at += 1;
            }
            Kind::Plain
         },
      };
      match runs.last_mut() {
         Some(last) if last.0 == kind && last.2 == start => last.2 = at,
         _ => runs.push((kind, start, at)),
      }
   }
   runs
      .into_iter()
      .map(|(kind, start, end)| (kind, &source[start..end]))
      .collect()
}

#[cfg(test)]
mod tests {
   use super::*;

   #[test]
   fn covers_the_source_and_colors_tokens() {
      let source = "const a = \"x\\\"y\"; // hé\nlet n = 1.5 /* c */ + `t` + 'é\\é';\nreturn ünï;";
      let runs = js(source);
      assert_eq!(runs.iter().map(|(_, run)| *run).collect::<String>(), source);
      let kind = |text: &str| runs.iter().find(|(_, run)| *run == text).map(|(kind, _)| *kind);
      assert_eq!(kind("const"), Some(Kind::Keyword));
      assert_eq!(kind("\"x\\\"y\""), Some(Kind::String));
      assert_eq!(kind("// hé"), Some(Kind::Comment));
      assert_eq!(kind("1.5"), Some(Kind::Number));
      assert_eq!(kind("/* c */"), Some(Kind::Comment));
      assert_eq!(kind("'é\\é'"), Some(Kind::String));
      assert_eq!(js("\"unterminated \\"), vec![(Kind::String, "\"unterminated \\")]);
   }
}
