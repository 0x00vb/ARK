//! HTML to readable text, without a DOM: enough to read an article, a docs page or a result
//! page. Only the page's `<main>` (or `<article>`) is read when it has one. Scripts, styles and
//! page furniture (headers, navigation, footers, forms) are dropped; blocks
//! become lines (no blank ones), headings `#` lines and list items `-` lines; links are
//! collected apart.

/// A page as text: its title, its lines, and the links it holds.
#[derive(Debug, Default, PartialEq)]
pub struct Page {
    pub title: String,
    pub text: String,
    pub links: Vec<Link>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Link {
    pub text: String,
    pub url: String,
}

/// Elements whose content is never text for a reader.
const SKIP: &[&str] = &[
    "script", "style", "noscript", "svg", "template", "head", "header", "nav", "footer", "aside",
    "form", "iframe", "select", "button", "dialog", "menu",
];

/// Elements that end a line.
const BLOCK: &[&str] = &[
    "p",
    "div",
    "br",
    "li",
    "tr",
    "ul",
    "ol",
    "table",
    "section",
    "article",
    "main",
    "pre",
    "blockquote",
    "hr",
    "dt",
    "dd",
    "figcaption",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
];

/// Read `html` as a page. `base` resolves relative links; links that are not http(s) are left out.
pub fn read(html: &str, base: Option<&reqwest::Url>) -> Page {
    let title = element_text(html, "title").unwrap_or_default();
    let html = content(html);
    let mut out = String::new();
    let mut links = Vec::new();
    let mut link: Option<(String, usize)> = None;
    let lower = html.to_ascii_lowercase();
    let mut i = 0;
    while i < html.len() {
        let Some(off) = html[i..].find('<') else {
            out.push_str(&html[i..]);
            break;
        };
        out.push_str(&html[i..i + off]);
        i += off;
        if lower[i..].starts_with("<!--") {
            i = lower[i..].find("-->").map_or(html.len(), |e| i + e + 3);
            continue;
        }
        let Some(end) = tag_end(html, i) else {
            break;
        };
        let tag = &html[i + 1..end];
        let closing = tag.starts_with('/');
        let name = tag_name(tag);
        i = end + 1;
        if name.is_empty() {
            continue;
        }
        if !closing && SKIP.contains(&name.as_str()) {
            let close = format!("</{name}");
            i = lower[i..].find(&close).map_or(html.len(), |e| {
                let at = i + e;
                tag_end(html, at).map_or(html.len(), |x| x + 1)
            });
            continue;
        }
        match name.as_str() {
            "a" if !closing => {
                link = attr(tag, "href").map(|h| (decode(&h), out.len()));
            }
            "a" => {
                if let Some((href, from)) = link.take() {
                    let text = squash(&decode(&out[from.min(out.len())..]));
                    if let Some(url) = absolute(&href, base) {
                        if !text.is_empty() && !links.iter().any(|l: &Link| l.url == url) {
                            links.push(Link { text, url });
                        }
                    }
                }
            }
            "li" if !closing => out.push_str("\n- "),
            h if !closing && h.len() == 2 && h.starts_with('h') && h != "hr" && h != "br" => {
                let level = h[1..].parse::<usize>().unwrap_or(1);
                out.push('\n');
                out.push_str(&"#".repeat(level));
                out.push(' ');
            }
            "td" | "th" if !closing => out.push(' '),
            n if BLOCK.contains(&n) => out.push('\n'),
            _ => {}
        }
    }
    Page {
        title: squash(&decode(&title)),
        text: tidy(&decode(&out)),
        links,
    }
}

/// The page's own content: its `<main>` element, else its `<article>`, else the whole page.
/// Site menus, language lists and banners usually sit outside both.
fn content(html: &str) -> &str {
    let lower = html.to_ascii_lowercase();
    for name in ["main", "article"] {
        let Some(open) = find_tag(&lower, name) else {
            continue;
        };
        if let Some(close) = lower.rfind(&format!("</{name}")) {
            if close > open {
                return &html[open..close];
            }
        }
    }
    html
}

/// Where the first `<name ...>` or `<name>` tag starts (not `<names`).
fn find_tag(lower: &str, name: &str) -> Option<usize> {
    let pat = format!("<{name}");
    let mut from = 0;
    while let Some(at) = lower[from..].find(&pat) {
        let at = from + at;
        match lower.as_bytes().get(at + pat.len()) {
            Some(b'>' | b' ' | b'\n' | b'\t' | b'\r' | b'/') => return Some(at),
            _ => from = at + pat.len(),
        }
    }
    None
}

/// The text inside the first `<name>` element, tags removed.
pub fn element_text(html: &str, name: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let open = find_tag(&lower, name)?;
    let start = tag_end(html, open)? + 1;
    let stop = lower[start..].find(&format!("</{name}"))? + start;
    Some(strip_tags(&html[start..stop]))
}

/// Text with every tag removed and entities decoded, on one line.
pub fn strip_tags(html: &str) -> String {
    let mut out = String::new();
    let mut i = 0;
    while let Some(off) = html[i..].find('<') {
        out.push_str(&html[i..i + off]);
        match tag_end(html, i + off) {
            Some(e) => i = e + 1,
            None => return squash(&decode(&out)),
        }
    }
    out.push_str(&html[i..]);
    squash(&decode(&out))
}

/// The value of attribute `name` in a tag's source (`a href="x" class=y`).
pub fn attr(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let mut from = 0;
    while let Some(at) = lower[from..].find(name) {
        let at = from + at;
        from = at + name.len();
        let before = lower[..at].chars().last();
        if !matches!(before, Some(c) if c.is_whitespace()) {
            continue;
        }
        let rest = tag[from..].trim_start();
        let Some(rest) = rest.strip_prefix('=') else {
            continue;
        };
        let rest = rest.trim_start();
        let value = match rest.chars().next() {
            Some(q @ ('"' | '\'')) => rest[1..].split(q).next().unwrap_or(""),
            _ => rest
                .split(|c: char| c.is_whitespace() || c == '>')
                .next()
                .unwrap_or(""),
        };
        return Some(decode(value));
    }
    None
}

/// Where the tag opening at `start` ends (the index of its `>`), quotes respected.
fn tag_end(html: &str, start: usize) -> Option<usize> {
    let mut quote: Option<u8> = None;
    for (k, b) in html.as_bytes()[start..].iter().enumerate() {
        match (quote, b) {
            (Some(q), c) if *c == q => quote = None,
            (Some(_), _) => {}
            (None, b'"' | b'\'') => quote = Some(*b),
            (None, b'>') => return Some(start + k),
            _ => {}
        }
    }
    None
}

fn tag_name(tag: &str) -> String {
    tag.trim_start_matches('/')
        .split(|c: char| c.is_whitespace() || c == '/' || c == '>')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
}

/// `href` as an absolute http(s) URL.
fn absolute(href: &str, base: Option<&reqwest::Url>) -> Option<String> {
    let url = match base {
        Some(b) => b.join(href.trim()).ok()?,
        None => reqwest::Url::parse(href.trim()).ok()?,
    };
    matches!(url.scheme(), "http" | "https").then(|| url.to_string())
}

/// Decode the entities a page commonly uses: named basics and numeric ones.
pub fn decode(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let Some(semi) = rest.bytes().take(12).position(|b| b == b';') else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let entity = &rest[1..semi];
        let ch = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" => Some(' '),
            "mdash" => Some('-'),
            "ndash" => Some('-'),
            "hellip" => Some('…'),
            "deg" => Some('°'),
            "middot" => Some('·'),
            "copy" => Some('©'),
            "rsquo" | "lsquo" => Some('\''),
            "rdquo" | "ldquo" => Some('"'),
            e if e.starts_with("#x") || e.starts_with("#X") => u32::from_str_radix(&e[2..], 16)
                .ok()
                .and_then(char::from_u32),
            e if e.starts_with('#') => e[1..].parse().ok().and_then(char::from_u32),
            _ => None,
        };
        match ch {
            Some(c) => {
                out.push(c);
                rest = &rest[semi + 1..];
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

/// One line: runs of whitespace become one space.
fn squash(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Lines trimmed and squashed; empty lines, empty list items and bare heading marks dropped.
fn tidy(s: &str) -> String {
    s.lines()
        .map(squash)
        .filter(|l| !l.is_empty() && l != "-" && !l.chars().all(|c| c == '#'))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_page_reads_as_lines_without_scripts_or_furniture() {
        let html = r#"<!doctype html><html><head><title>Tea &amp; Time</title>
            <style>p { color: red }</style><script>var x = "<p>no</p>";</script></head>
            <body><nav><a href="/home">Home</a></nav>
            <h1>Brewing</h1><p>Steep for <b>three</b> minutes.<br>Then pour.</p>
            <ul><li>Green: 80&#176;C</li><li>Black: 95&deg;C</li></ul>
            <!-- a comment <p>hidden</p> -->
            <p>See <a href="guide.html?a=1&amp;b=2">the guide</a> or <a href="mailto:x@y">mail</a>.</p>
            <footer>Copyright</footer></body></html>"#;
        let base = reqwest::Url::parse("https://tea.example/brew/").unwrap();
        let page = read(html, Some(&base));
        assert_eq!(page.title, "Tea & Time");
        assert_eq!(
            page.text,
            "# Brewing\nSteep for three minutes.\nThen pour.\n- Green: 80°C\n- Black: 95°C\nSee the guide or mail."
        );
        assert_eq!(
            page.links,
            vec![Link {
                text: "the guide".into(),
                url: "https://tea.example/brew/guide.html?a=1&b=2".into()
            }]
        );
    }

    #[test]
    fn only_the_main_content_is_read_when_the_page_marks_it() {
        let html = "<title>T</title><div>Menu<ul><li>Deutsch</li></ul></div><main id=c><h1>Tea</h1><p>Body</p></main><div>Related</div>";
        assert_eq!(read(html, None).text, "# Tea\nBody");
        assert_eq!(read(html, None).title, "T");
        assert_eq!(read("<mainframe>x</mainframe><p>y</p>", None).text, "x\ny");
    }

    #[test]
    fn attributes_are_read_with_or_without_quotes() {
        let tag = r#"a class="x" href='/p?q=1' data-href="no" title=plain"#;
        assert_eq!(attr(tag, "href").as_deref(), Some("/p?q=1"));
        assert_eq!(attr(tag, "title").as_deref(), Some("plain"));
        assert_eq!(attr(tag, "id"), None);
        assert_eq!(strip_tags("<b>a</b>  <i>b &lt;c&gt;</i>"), "a b <c>");
        assert_eq!(decode("a &…, b&c &amp"), "a &…, b&c &amp");
    }

    #[test]
    fn a_quoted_angle_bracket_does_not_end_a_tag() {
        let page = read(r#"<p title="a > b">kept</p>"#, None);
        assert_eq!(page.text, "kept");
    }
}
