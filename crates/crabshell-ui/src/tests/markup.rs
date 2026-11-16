//! The notification markup parser.

use crate::{MARKUP_CAP, Span, parse_markup};

fn plain(text: &str) -> Vec<Span> {
  vec![Span::new(text)]
}

#[test]
fn each_tag_styles_its_text() {
  assert_eq!(
    parse_markup("a <b>bold</b> <i>it</i> <u>under</u>"),
    vec![
      Span::new("a "),
      Span::new("bold").bold(),
      Span::new(" "),
      Span::new("it").italic(),
      Span::new(" "),
      Span::new("under").underline(),
    ]
  );
  assert_eq!(
    parse_markup(r#"see <a href="https://example.org/?a=1&amp;b=2">here</a>."#),
    vec![
      Span::new("see "),
      Span::new("here")
        .underline()
        .link("https://example.org/?a=1&b=2"),
      Span::new("."),
    ]
  );
}

#[test]
fn tags_nest_in_any_order() {
  assert_eq!(
    parse_markup("<b>x<i>y<u>z</u></i></b><i><b>w</b></i>"),
    vec![
      Span::new("x").bold(),
      Span::new("y").bold().italic(),
      Span::new("z").bold().italic().underline(),
      Span::new("w").bold().italic(),
    ]
  );
  assert_eq!(
    parse_markup("<a href='mailto:me@example.org'><b>mail</b> me</a>"),
    vec![
      Span::new("mail")
        .bold()
        .underline()
        .link("mailto:me@example.org"),
      Span::new(" me").underline().link("mailto:me@example.org"),
    ]
  );
  // Tag names and attribute names take any case.
  assert_eq!(
    parse_markup("<B>x</b><A HREF=\"HTTP://e.org\">y</A>"),
    vec![
      Span::new("x").bold(),
      Span::new("y").underline().link("HTTP://e.org")
    ]
  );
}

#[test]
fn entities_and_character_references_decode() {
  assert_eq!(
    parse_markup("&lt;&gt;&amp;&quot;&apos; &#65;&#x42;&#X1F600;"),
    plain("<>&\"' AB😀")
  );
  // Bad ones stay as written.
  assert_eq!(
    parse_markup("&nbsp; &#; &#xZZ; &#+65; &#0; &#1114112; & &amp"),
    plain("&nbsp; &#; &#xZZ; &#+65; &#0; &#1114112; & &amp")
  );
}

#[test]
fn unknown_tags_go_and_their_text_stays() {
  assert_eq!(
    parse_markup("<span color='red'>hi</span> <br/>there<script>x</script>"),
    plain("hi therex")
  );
  // A self-closed tag does nothing.
  assert_eq!(parse_markup("<b/>x<a href='https://e.org'/>y"), plain("xy"));
}

#[test]
fn malformed_markup_degrades_to_text() {
  assert_eq!(parse_markup("1 < 2 > 0 <3 <"), plain("1 < 2 > 0 <3 <"));
  assert_eq!(parse_markup("<b x\"y\">z"), plain("<b x\"y\">z"));
  assert_eq!(
    parse_markup("<a href=\"https://e.org"),
    plain("<a href=\"https://e.org")
  );
  // A close nothing opened is dropped; a tag left open lasts to the end.
  assert_eq!(
    parse_markup("a</b>b<i>c"),
    vec![Span::new("ab"), Span::new("c").italic()]
  );
  // Closing an outer tag closes the ones left open inside it.
  assert_eq!(
    parse_markup("<b><i>x</b>y</i>"),
    vec![Span::new("x").bold().italic(), Span::new("y")]
  );
}

#[test]
fn links_need_a_web_or_mail_scheme() {
  for href in [
    "javascript:alert(1)",
    "file:///etc/passwd",
    "/relative",
    "",
    " https://e.org",
  ] {
    assert_eq!(
      parse_markup(&format!("<a href=\"{href}\">x</a>")),
      plain("x"),
      "{href}"
    );
  }
  assert_eq!(parse_markup("<a>x</a>"), plain("x"));
  assert_eq!(
    parse_markup("<a href='HtTpS://e.org'>x</a>"),
    vec![Span::new("x").underline().link("HtTpS://e.org")]
  );
}

#[test]
fn images_show_their_alt_text_only() {
  assert_eq!(
    parse_markup(r#"a <img src="/etc/shadow" alt="a &lt;cat&gt;"/> b <img src="x.png"> c"#),
    plain("a a <cat> b  c")
  );
  assert_eq!(
    parse_markup("<b><img alt='pic'/></b>"),
    vec![Span::new("pic").bold()]
  );
}

#[test]
fn line_breaks_stay() {
  assert_eq!(
    parse_markup("one\n<b>two\nthree</b>"),
    vec![Span::new("one\n"), Span::new("two\nthree").bold()]
  );
}

#[test]
fn a_long_body_is_cut_at_a_char_boundary() {
  // "é" is two bytes, so the cap falls inside one.
  let body = format!("a{}", "é".repeat(MARKUP_CAP));
  let spans = parse_markup(&body);
  assert_eq!(spans.len(), 1);
  assert_eq!(spans[0].text.len(), MARKUP_CAP - 1);
  assert!(spans[0].text.ends_with('é'));
}

#[test]
fn random_bytes_never_panic() {
  // A small xorshift, so the run is the same each time.
  let mut seed = 0x2545_f491_4f6c_dd1d_u64;
  let mut next = move || {
    seed ^= seed << 13;
    seed ^= seed >> 7;
    seed ^= seed << 17;
    seed
  };
  const PIECES: [&str; 16] = [
    "<", ">", "/", "&", ";", "#", "x", "\"", "'", "=", " ", "b", "a href", "img alt", "é", "&amp;",
  ];
  for _ in 0..4000 {
    let len = (next() % 40) as usize;
    let body: String = (0..len)
      .map(|_| match next() % 3 {
        0 => char::from_u32((next() % 0x250) as u32)
          .unwrap_or('?')
          .to_string(),
        _ => PIECES[(next() % 16) as usize].to_owned(),
      })
      .collect();
    let spans = parse_markup(&body);
    assert!(spans.iter().all(|span| !span.text.is_empty()), "{body:?}");
    // Text with nothing special comes back whole.
    if !body.contains(['<', '&']) && !body.is_empty() {
      assert_eq!(spans, plain(&body));
    }
  }
}

#[test]
fn many_stray_brackets_parse_quickly() {
  let start = std::time::Instant::now();
  for body in [
    "<".repeat(MARKUP_CAP),
    "&".repeat(MARKUP_CAP),
    "<a href=\"".repeat(MARKUP_CAP / 9),
    "<b ".repeat(MARKUP_CAP / 3),
  ] {
    parse_markup(&body);
  }
  // Reading each stray bracket to the end would take seconds.
  assert!(start.elapsed().as_millis() < 1000, "{:?}", start.elapsed());
}

#[test]
fn deep_nesting_parses_quickly() {
  let long_link = format!(
    "<a href=\"https://{}\">{}",
    "x".repeat(MARKUP_CAP / 2),
    "&amp;".repeat(MARKUP_CAP / 12)
  );
  for body in [
    "<b>".repeat(10922) + &"<".repeat(32768),
    "<b>x".repeat(16384),
    "<a>".repeat(10922) + &"&".repeat(32768),
    "<i>".repeat(10922) + &"</b>".repeat(8192),
    long_link,
  ] {
    let start = std::time::Instant::now();
    parse_markup(&body);
    // Scanning the open tags for each piece of text took seconds.
    assert!(
      start.elapsed().as_millis() < 300,
      "{:?} for {:?}…",
      start.elapsed(),
      &body[..20]
    );
  }
}

#[test]
fn a_close_tag_closes_what_opened_inside_it_at_any_depth() {
  let body = "<a href='https://1.org'><b><a href='https://2.org'><i>\
    <a href='https://3.org'><u>x</a>y</a>z</b>w";
  assert_eq!(
    parse_markup(body),
    vec![
      Span::new("x")
        .bold()
        .italic()
        .underline()
        .link("https://3.org"),
      Span::new("y")
        .bold()
        .italic()
        .underline()
        .link("https://2.org"),
      Span::new("z").bold().underline().link("https://1.org"),
      Span::new("w").underline().link("https://1.org"),
    ]
  );
}
