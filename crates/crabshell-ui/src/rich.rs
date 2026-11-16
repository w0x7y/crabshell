//! Styled spans for [`Text::rich`](crate::Text::rich), and the markup
//! subset of the Desktop Notifications spec, parsed into spans.

use std::sync::atomic::{AtomicBool, Ordering};

use crate::style::Color;

/// A run of text in a [`Text::rich`](crate::Text::rich), with its own
/// settings. What a span doesn't set comes from the text: its inherited
/// font, color and decorations.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Span {
  /// The text. A line break in it starts a new line.
  pub text: String,
  /// Weight 700 on the text's font.
  pub bold: bool,
  /// The text's font, italic.
  pub italic: bool,
  /// A line under the span.
  pub underline: bool,
  /// A line through the span.
  pub strikethrough: bool,
  /// The span's color, or the text's for `None`.
  pub color: Option<Color>,
  /// Where the span links to. It draws as its other settings say.
  pub link: Option<String>,
}

impl Span {
  /// A span showing `text` as the text around it does.
  pub fn new(text: impl Into<String>) -> Span {
    Span {
      text: text.into(),
      ..Span::default()
    }
  }

  /// Draws the span in weight 700.
  pub fn bold(mut self) -> Span {
    self.bold = true;
    self
  }

  /// Draws the span in the italic face.
  pub fn italic(mut self) -> Span {
    self.italic = true;
    self
  }

  /// Draws a line under the span.
  pub fn underline(mut self) -> Span {
    self.underline = true;
    self
  }

  /// Draws a line through the span.
  pub fn strikethrough(mut self) -> Span {
    self.strikethrough = true;
    self
  }

  /// Draws the span in `color`.
  pub fn color(mut self, color: Color) -> Span {
    self.color = Some(color);
    self
  }

  /// Makes the span a link to `url`.
  pub fn link(mut self, url: impl Into<String>) -> Span {
    self.link = Some(url.into());
    self
  }

  /// Whether `other` looks the same and links to the same place.
  fn same_style(&self, other: &Span) -> bool {
    (self.bold, self.italic, self.underline, self.strikethrough)
      == (
        other.bold,
        other.italic,
        other.underline,
        other.strikethrough,
      )
      && self.color == other.color
      && self.link == other.link
  }
}

/// The longest body [`parse_markup`] reads, in bytes.
pub const MARKUP_CAP: usize = 64 * 1024;

/// Turns a notification body in the Desktop Notifications spec's markup
/// into spans: `<b>`, `<i>`, `<u>`, `<a href="...">` and
/// `<img src="..." alt="..."/>`, nested in any order, with the five XML
/// entities and numeric character references. It never reads general
/// HTML.
///
/// Unknown tags are dropped and their text kept. A `<` that doesn't start
/// a tag, a bad entity and a close tag nothing opened stay or go as plain
/// text; a tag left open lasts to the end. An `img` shows its `alt`, and
/// its `src` is never read. A link is kept only for an `href` starting
/// with `http:`, `https:` or `mailto:`; any other `a` is plain text, since
/// a notification can come from any program. Links are underlined. A body
/// over [`MARKUP_CAP`] bytes is cut before it is read, logged once.
pub fn parse_markup(body: &str) -> Vec<Span> {
  static CUT_LOGGED: AtomicBool = AtomicBool::new(false);
  let mut body = body;
  if body.len() > MARKUP_CAP {
    let mut end = MARKUP_CAP;
    while !body.is_char_boundary(end) {
      end -= 1;
    }
    if !CUT_LOGGED.swap(true, Ordering::Relaxed) {
      tracing::warn!(
        bytes = body.len(),
        "a markup body is over 64 KiB, so only its first 64 KiB are shown"
      );
    }
    body = &body[..end];
  }
  let mut out = Markup::default();
  let mut rest = body;
  while let Some(at) = rest.find(['<', '&']) {
    out.text(&rest[..at]);
    rest = &rest[at..];
    let used = if rest.starts_with('&') {
      entity(rest).map(|(ch, len)| {
        out.push(ch.encode_utf8(&mut [0; 4]));
        len
      })
    } else {
      tag(rest).map(|(tag, len)| {
        out.tag(tag);
        len
      })
    };
    // Not an entity or a tag: the character is plain text.
    let used = used.unwrap_or_else(|| {
      out.push(&rest[..1]);
      1
    });
    rest = &rest[used..];
  }
  out.text(rest);
  out.spans
}

/// The spans so far and the tags open.
#[derive(Default)]
struct Markup {
  spans: Vec<Span>,
  /// Each open `b`, `i`, `u` or `a`, by name, with an `a`'s link.
  open: Vec<(&'static str, Option<String>)>,
}

impl Markup {
  /// Adds text, unless it is empty.
  fn text(&mut self, text: &str) {
    if !text.is_empty() {
      self.push(text);
    }
  }

  /// Adds text in the open tags' style, joined to the last span if it
  /// looks the same.
  fn push(&mut self, text: &str) {
    let has = |name| self.open.iter().any(|(open, _)| *open == name);
    let link = self.open.iter().rev().find_map(|(name, link)| match *name {
      "a" => Some(link.clone()),
      _ => None,
    });
    let span = Span {
      text: String::new(),
      bold: has("b"),
      italic: has("i"),
      underline: has("u") || matches!(link, Some(Some(_))),
      strikethrough: false,
      color: None,
      link: link.flatten(),
    };
    match self.spans.last_mut() {
      Some(last) if last.same_style(&span) => last.text.push_str(text),
      _ => self.spans.push(Span {
        text: text.to_owned(),
        ..span
      }),
    }
  }

  fn tag(&mut self, tag: Tag) {
    match tag {
      Tag::Open(name, link) => self.open.push((name, link)),
      // Closing a tag closes any opened inside it and left open.
      Tag::Close(name) => {
        if let Some(at) = self.open.iter().rposition(|(open, _)| *open == name) {
          self.open.truncate(at);
        }
      }
      Tag::Image(alt) => self.text(&alt),
      Tag::Unknown => {}
    }
  }
}

enum Tag {
  Open(&'static str, Option<String>),
  Close(&'static str),
  Image(String),
  Unknown,
}

/// Reads the tag `text` starts with, and how many bytes it takes; `None`
/// if the `<` doesn't start one.
fn tag(text: &str) -> Option<(Tag, usize)> {
  let bytes = text.as_bytes();
  let mut at = 1;
  let close = bytes.get(at) == Some(&b'/');
  if close {
    at += 1;
  }
  let name_start = at;
  if !bytes.get(at)?.is_ascii_alphabetic() {
    return None;
  }
  while bytes.get(at).is_some_and(u8::is_ascii_alphanumeric) {
    at += 1;
  }
  let name = text[name_start..at].to_ascii_lowercase();
  let mut attributes = Vec::new();
  let mut empty = false;
  loop {
    while bytes.get(at).is_some_and(u8::is_ascii_whitespace) {
      at += 1;
    }
    match bytes.get(at)? {
      b'>' => {
        at += 1;
        break;
      }
      b'/' if bytes.get(at + 1) == Some(&b'>') => {
        at += 2;
        empty = true;
        break;
      }
      // Attributes need whitespace after the name or the last value.
      _ if !bytes[at - 1].is_ascii_whitespace() => return None,
      _ => {
        let (attribute, len) = attribute(&text[at..])?;
        attributes.push(attribute);
        at += len;
      }
    }
  }
  let value = |key: &str| {
    attributes
      .iter()
      .find(|(name, _)| name.eq_ignore_ascii_case(key))
      .map(|(_, value)| value.clone())
  };
  let tag = match (close, name.as_str()) {
    (false, "img") => Tag::Image(value("alt").unwrap_or_default()),
    // `<b/>` opens and closes at once, so it does nothing.
    _ if empty => Tag::Unknown,
    (false, "a") => Tag::Open("a", value("href").filter(|href| safe_link(href))),
    (false, "b") => Tag::Open("b", None),
    (false, "i") => Tag::Open("i", None),
    (false, "u") => Tag::Open("u", None),
    (true, "a") => Tag::Close("a"),
    (true, "b") => Tag::Close("b"),
    (true, "i") => Tag::Close("i"),
    (true, "u") => Tag::Close("u"),
    _ => Tag::Unknown,
  };
  Some((tag, at))
}

/// Reads `name="value"`, `name='value'` or `name=value` with its entities
/// replaced, or a bare `name`. A `<` or the end of the text ends it with
/// `None`, so a stray `<` is read once, never again for each one after it.
fn attribute(text: &str) -> Option<((String, String), usize)> {
  let name_len = text
    .find(|ch: char| ch.is_ascii_whitespace() || matches!(ch, '=' | '>' | '/' | '<' | '"' | '\''))
    .unwrap_or(text.len());
  if name_len == 0 {
    return None;
  }
  let name = text[..name_len].to_owned();
  let rest = &text[name_len..];
  let Some(rest) = rest.strip_prefix('=') else {
    return Some(((name, String::new()), name_len));
  };
  let (raw, len) = match rest.chars().next()? {
    quote @ ('"' | '\'') => {
      let end = rest[1..].find([quote, '<'])? + 1;
      if !rest[end..].starts_with(quote) {
        return None;
      }
      (&rest[1..end], end + 1)
    }
    _ => {
      let end = rest
        .find(|ch: char| ch.is_ascii_whitespace() || matches!(ch, '>' | '<' | '"' | '\''))
        .unwrap_or(rest.len());
      (&rest[..end], end)
    }
  };
  if rest[len..].is_empty() {
    return None;
  }
  Some(((name, decode(raw)), name_len + 1 + len))
}

/// `text` with its entities replaced; a bad one stays as written.
fn decode(text: &str) -> String {
  let mut out = String::with_capacity(text.len());
  let mut rest = text;
  while let Some(at) = rest.find('&') {
    out.push_str(&rest[..at]);
    rest = &rest[at..];
    match entity(rest) {
      Some((ch, len)) => {
        out.push(ch);
        rest = &rest[len..];
      }
      None => {
        out.push('&');
        rest = &rest[1..];
      }
    }
  }
  out.push_str(rest);
  out
}

/// Reads the entity `text` starts with, and how many bytes it takes: one
/// of the five XML ones, or `&#N;` or `&#xN;` naming a character.
fn entity(text: &str) -> Option<(char, usize)> {
  // The longest is `&#x10FFFF;`; a short look keeps a body of stray `&`s
  // from being read again for each one.
  let end = text.bytes().take(12).position(|byte| byte == b';')?;
  let ch = match &text[1..end] {
    "amp" => '&',
    "lt" => '<',
    "gt" => '>',
    "quot" => '"',
    "apos" => '\'',
    name => {
      let number = name.strip_prefix('#')?;
      let (digits, radix) = match number.strip_prefix(['x', 'X']) {
        Some(hex) => (hex, 16),
        None => (number, 10),
      };
      // `from_str_radix` would take a leading `+`.
      if !digits.chars().all(|ch| ch.is_digit(radix)) {
        return None;
      }
      let code = u32::from_str_radix(digits, radix).ok()?;
      // NUL isn't a character XML allows.
      char::from_u32(code).filter(|&ch| ch != '\0')?
    }
  };
  Some((ch, end + 1))
}

/// Whether `href` is a link a notification may hand out: `http:`,
/// `https:` or `mailto:`, in any case.
fn safe_link(href: &str) -> bool {
  ["http:", "https:", "mailto:"].iter().any(|scheme| {
    href
      .get(..scheme.len())
      .is_some_and(|start| start.eq_ignore_ascii_case(scheme))
  })
}
