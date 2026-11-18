//! SPA PODs, the values PipeWire's native protocol carries, and the
//! 16-byte header in front of each message. Everything is in the
//! machine's own byte order, as the protocol is.

/// A message's header: the object it is for, its opcode, its size past
/// the header, its sequence number and how many fds came with it.
pub(crate) const HEADER: usize = 16;

/// The longest message taken; a longer one drops the connection.
pub(crate) const MAX_MESSAGE: usize = 1024 * 1024;

const NONE: u32 = 1;
const BOOL: u32 = 2;
const ID: u32 = 3;
const INT: u32 = 4;
const LONG: u32 = 5;
const STRING: u32 = 8;
const STRUCT: u32 = 14;

/// Why bytes aren't a POD, or not the one expected, for a log line.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Bad(pub(crate) &'static str);

/// One POD, read without copying.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Pod<'a> {
  None,
  Bool(bool),
  Id(u32),
  Int(i32),
  Long(i64),
  /// Without its closing NUL.
  String(&'a [u8]),
  Struct(Fields<'a>),
  /// A type nothing here reads, such as a format's object.
  Other(u32),
}

/// A dictionary's key and value.
pub(crate) type Entry<'a> = (&'a [u8], &'a [u8]);

/// A struct's fields, read one at a time.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Fields<'a>(&'a [u8]);

/// The POD at the start of `bytes`, and how many bytes it takes with its
/// padding to 8.
pub(crate) fn parse(bytes: &[u8]) -> Result<(Pod<'_>, usize), Bad> {
  let (Some(size), Some(kind)) = (word(bytes, 0), word(bytes, 4)) else {
    return Err(Bad("a POD cut short in its header"));
  };
  let size = size as usize;
  let Some(body) = bytes.get(8..).and_then(|rest| rest.get(..size)) else {
    return Err(Bad("a POD longer than its message"));
  };
  let fixed = |len| body.get(..len).ok_or(Bad("a POD too short for its type"));
  let pod = match kind {
    NONE => Pod::None,
    BOOL => Pod::Bool(u32::from_ne_bytes(array(fixed(4)?)) != 0),
    ID => Pod::Id(u32::from_ne_bytes(array(fixed(4)?))),
    INT => Pod::Int(i32::from_ne_bytes(array(fixed(4)?))),
    LONG => Pod::Long(i64::from_ne_bytes(array(fixed(8)?))),
    STRING => match body.split_last() {
      Some((0, text)) => Pod::String(text),
      _ => return Err(Bad("a string with no closing NUL")),
    },
    STRUCT => Pod::Struct(Fields(body)),
    other => Pod::Other(other),
  };
  // The last field of a struct may come without its padding.
  let taken = (8 + size).next_multiple_of(8).min(bytes.len());
  Ok((pod, taken))
}

impl<'a> Fields<'a> {
  /// The fields of the struct at the start of `bytes`, a message's body.
  pub(crate) fn of(bytes: &'a [u8]) -> Result<Fields<'a>, Bad> {
    match parse(bytes)?.0 {
      Pod::Struct(fields) => Ok(fields),
      _ => Err(Bad("a message that isn't a struct")),
    }
  }

  /// The next field.
  pub(crate) fn next(&mut self) -> Result<Pod<'a>, Bad> {
    if self.0.is_empty() {
      return Err(Bad("a struct with a field missing"));
    }
    let (pod, taken) = parse(self.0)?;
    self.0 = &self.0[taken..];
    Ok(pod)
  }

  /// The next field, an `Int` that holds an id or a count.
  pub(crate) fn uint(&mut self) -> Result<u32, Bad> {
    match self.next()? {
      Pod::Int(value) => Ok(value.cast_unsigned()),
      _ => Err(Bad("a field that isn't an Int")),
    }
  }

  pub(crate) fn int(&mut self) -> Result<i32, Bad> {
    match self.next()? {
      Pod::Int(value) => Ok(value),
      _ => Err(Bad("a field that isn't an Int")),
    }
  }

  pub(crate) fn long(&mut self) -> Result<i64, Bad> {
    match self.next()? {
      Pod::Long(value) => Ok(value),
      _ => Err(Bad("a field that isn't a Long")),
    }
  }

  /// The next field, a string or `None` for no string.
  pub(crate) fn text(&mut self) -> Result<Option<&'a [u8]>, Bad> {
    match self.next()? {
      Pod::String(text) => Ok(Some(text)),
      Pod::None => Ok(None),
      _ => Err(Bad("a field that isn't a String")),
    }
  }

  pub(crate) fn fields(&mut self) -> Result<Fields<'a>, Bad> {
    match self.next()? {
      Pod::Struct(fields) => Ok(fields),
      _ => Err(Bad("a field that isn't a Struct")),
    }
  }

  /// The next field, a dictionary: a struct of an `Int` count, then that
  /// many keys and values. A value that is `None` is left out.
  pub(crate) fn dict(&mut self) -> Result<Vec<Entry<'a>>, Bad> {
    let mut fields = self.fields()?;
    let count = fields.uint()?;
    let mut entries = Vec::new();
    for _ in 0..count {
      let key = fields.text()?.ok_or(Bad("a dictionary key that is None"))?;
      if let Some(value) = fields.text()? {
        entries.push((key, value));
      }
    }
    Ok(entries)
  }
}

fn word(bytes: &[u8], at: usize) -> Option<u32> {
  let bytes = bytes.get(at..at + 4)?;
  Some(u32::from_ne_bytes(array(bytes)))
}

fn array<const N: usize>(bytes: &[u8]) -> [u8; N] {
  bytes.try_into().expect("invariant: sliced to N bytes")
}

/// PODs to send, written one after another into a struct.
#[derive(Default)]
pub(crate) struct Builder(Vec<u8>);

impl Builder {
  pub(crate) fn int(mut self, value: i32) -> Builder {
    push(&mut self.0, INT, &value.to_ne_bytes());
    self
  }

  pub(crate) fn uint(self, value: u32) -> Builder {
    self.int(value.cast_signed())
  }

  pub(crate) fn string(mut self, text: &str) -> Builder {
    let mut body = text.as_bytes().to_vec();
    body.push(0);
    push(&mut self.0, STRING, &body);
    self
  }

  pub(crate) fn structure(mut self, fields: Builder) -> Builder {
    push(&mut self.0, STRUCT, &fields.0);
    self
  }

  /// The fields as a struct, a message's body.
  pub(crate) fn finish(self) -> Vec<u8> {
    let mut pod = Vec::new();
    push(&mut pod, STRUCT, &self.0);
    pod
  }
}

/// Appends a POD of `kind` holding `body`, padded to 8.
fn push(out: &mut Vec<u8>, kind: u32, body: &[u8]) {
  let size = u32::try_from(body.len()).expect("invariant: PODs sent are small");
  out.extend_from_slice(&size.to_ne_bytes());
  out.extend_from_slice(&kind.to_ne_bytes());
  out.extend_from_slice(body);
  out.resize(out.len().next_multiple_of(8), 0);
}

/// A message's header.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Header {
  pub(crate) id: u32,
  pub(crate) opcode: u8,
  /// Of the body, past the header.
  pub(crate) size: usize,
}

/// The header at the start of `bytes`, once all 16 bytes are there.
pub(crate) fn header(bytes: &[u8]) -> Option<Header> {
  let (id, packed) = (word(bytes, 0)?, word(bytes, 4)?);
  word(bytes, 12)?;
  Some(Header {
    id,
    opcode: (packed >> 24) as u8,
    size: (packed & 0x00ff_ffff) as usize,
  })
}

/// Appends a message for `id` with `opcode` and `body`, numbered `seq`.
pub(crate) fn message(out: &mut Vec<u8>, id: u32, opcode: u8, seq: u32, body: &[u8]) {
  let size = u32::try_from(body.len()).expect("invariant: messages sent are small");
  for word in [id, u32::from(opcode) << 24 | size, seq, 0] {
    out.extend_from_slice(&word.to_ne_bytes());
  }
  out.extend_from_slice(body);
}
