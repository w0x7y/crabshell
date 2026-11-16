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
  /// A URL handed to the text's [`on_link`](crate::Text::on_link) when
  /// the span is clicked. It draws as its other settings say.
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

/// The spans so far and the tags open. Each open tag is kept as its kind,
/// and each kind keeps where its open tags are, so styling a piece of text
/// and closing a tag take the same time however deep the nesting goes.
#[derive(Default)]
struct Markup {
  spans: Vec<Span>,
  /// The kind of each open tag: `B`, `I`, `U` or `A`, outermost first.
  open: Vec<usize>,
  /// For each kind, where its open tags are in `open`.
  at: [Vec<usize>; 4],
  /// Each open `a`'s link, outermost first, with a number of its own.
  links: Vec<(usize, Option<String>)>,
  /// How many `a`s opened, which numbers them.
  opened: usize,
  /// The last span's bold, italic and underline, and the number of the
  /// `a` its link came from.
  last: Option<(bool, bool, bool, Option<usize>)>,
}

const B: usize = 0;
const I: usize = 1;
const U: usize = 2;
const A: usize = 3;

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
    let open = |kind: usize| !self.at[kind].is_empty();
    let (number, link) = match self.links.last() {
      Some((number, Some(link))) => (Some(*number), Some(link)),
      _ => (None, None),
    };
    let style = (open(B), open(I), open(U) || link.is_some(), number);
    let same = match (self.last, self.spans.last()) {
      (Some(last), _) if last == style => true,
      // Two links to the same place look the same; the text decides.
      (Some((bold, italic, underline, _)), Some(span)) => {
        (bold, italic, underline) == (style.0, style.1, style.2) && span.link.as_ref() == link
      }
      _ => false,
    };
    if let (true, Some(last)) = (same, self.spans.last_mut()) {
      last.text.push_str(text);
    } else {
      self.spans.push(Span {
        text: text.to_owned(),
        bold: style.0,
        italic: style.1,
        underline: style.2,
        strikethrough: false,
        color: None,
        link: link.cloned(),
      });
    }
    self.last = Some(style);
  }

  fn tag(&mut self, tag: Tag) {
    match tag {
      Tag::Open(kind, link) => {
        self.at[kind].push(self.open.len());
        self.open.push(kind);
        if kind == A {
          self.links.push((self.opened, link));
          self.opened += 1;
        }
      }
      // Closing a tag closes any opened inside it and left open.
      Tag::Close(kind) => {
        let Some(&from) = self.at[kind].last() else {
          return;
        };
        while self.open.len() > from {
          let closed = self.open.pop().expect("open holds the tag closed");
          self.at[closed].pop();
          if closed == A {
            self.links.pop();
          }
        }
      }
      Tag::Image(alt) => self.text(&alt),
      Tag::Unknown => {}
    }
  }
}

enum Tag {
  /// A kind, `B`, `I`, `U` or `A`, and an `a`'s link.
  Open(usize, Option<String>),
  Close(usize),
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
    (false, "a") => Tag::Open(A, value("href").filter(|href| safe_link(href))),
    (false, "b") => Tag::Open(B, None),
    (false, "i") => Tag::Open(I, None),
    (false, "u") => Tag::Open(U, None),
    (true, "a") => Tag::Close(A),
    (true, "b") => Tag::Close(B),
    (true, "i") => Tag::Close(I),
    (true, "u") => Tag::Close(U),
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
