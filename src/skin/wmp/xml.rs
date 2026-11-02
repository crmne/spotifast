//! The tag reader for skin definitions.
//!
//! A `.wms` file is nearly XML. Skins written by hand get away with what a
//! strict parser would refuse, so this forgives: unquoted or missing values,
//! stray text, any case, comments, and an attribute given twice, where the
//! last one counts, as the skins that do it were tuned to.

/// The deepest element nesting kept; deeper elements are dropped. Building,
/// laying out, painting and hit testing a view all recurse this far on the
/// window's thread. Real skins nest about a dozen.
const MAX_DEPTH: usize = 32;
/// The most elements a definition may have.
const MAX_NODES: usize = 100_000;

/// An element of the definition.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Node {
    /// Lower case.
    pub name: String,
    /// Names are lower case: skins write them any way.
    pub attrs: Vec<(String, String)>,
    pub children: Vec<Node>,
}

impl Node {
    /// An attribute's value; the last one, when it is given twice.
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .rev()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    /// The first value of an attribute: `id` and `scriptFile`, which WMP
    /// reads by name, take the first where the rest take the last.
    pub fn first_attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    /// The first element of this name, here or below.
    pub fn find(&self, name: &str) -> Option<&Node> {
        if self.name == name {
            return Some(self);
        }
        self.children.iter().find_map(|child| child.find(name))
    }

    /// Calls `visit` on this element and every one below.
    pub fn walk<'a>(&'a self, visit: &mut impl FnMut(&'a Node)) {
        visit(self);
        for child in &self.children {
            child.walk(visit);
        }
    }
}

#[derive(Debug)]
pub struct Tag {
    pub name: String,
    pub closing: bool,
    pub self_closing: bool,
    pub attrs: Vec<(String, String)>,
}

/// Reads a document into its element tree, under a nameless root.
pub fn parse(text: &str) -> Node {
    let mut stack = vec![Node::default()];
    let mut count = 0usize;
    for tag in tags(text) {
        if tag.closing {
            if let Some(at) = stack.iter().rposition(|node| node.name == tag.name)
                && at > 0
            {
                while stack.len() > at {
                    close(&mut stack);
                }
            }
            continue;
        }
        count += 1;
        if count > MAX_NODES {
            break;
        }
        let node = Node {
            name: tag.name,
            attrs: tag.attrs,
            children: Vec::new(),
        };
        if tag.self_closing || stack.len() >= MAX_DEPTH {
            if let Some(parent) = stack.last_mut() {
                parent.children.push(node);
            }
        } else {
            stack.push(node);
        }
    }
    while stack.len() > 1 {
        close(&mut stack);
    }
    stack.pop().unwrap_or_default()
}

fn close(stack: &mut Vec<Node>) {
    if let Some(node) = stack.pop()
        && let Some(parent) = stack.last_mut()
    {
        parent.children.push(node);
    }
}

/// The tags of a document, tolerating what real skins do.
pub fn tags(text: &str) -> Vec<Tag> {
    let mut found = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find('<') {
        rest = &rest[start + 1..];
        if let Some(after) = rest.strip_prefix("!--") {
            rest = after.split_once("-->").map_or("", |(_, tail)| tail);
            continue;
        }
        if rest.starts_with('?') || rest.starts_with('!') {
            rest = rest.split_once('>').map_or("", |(_, tail)| tail);
            continue;
        }
        // The tag ends at the first `>` outside quotes.
        let mut quote = None;
        let mut end = rest.len();
        for (at, character) in rest.char_indices() {
            match (quote, character) {
                (None, '"' | '\'') => quote = Some(character),
                (Some(open), c) if c == open => quote = None,
                (None, '>') => {
                    end = at;
                    break;
                }
                _ => {}
            }
        }
        let body = &rest[..end];
        rest = rest.get(end + 1..).unwrap_or("");
        found.extend(parse_tag(body));
    }
    found
}

fn parse_tag(body: &str) -> Option<Tag> {
    let mut body = body.trim();
    let closing = body.starts_with('/');
    body = body.trim_start_matches('/').trim();
    let self_closing = body.ends_with('/');
    body = body.trim_end_matches('/').trim();
    let name_end = body.find(|c: char| c.is_whitespace()).unwrap_or(body.len());
    let name = body[..name_end].to_ascii_lowercase();
    if name.is_empty() {
        return None;
    }
    let mut attrs = Vec::new();
    let mut rest = &body[name_end..];
    loop {
        rest = rest.trim_start();
        let key_end = rest
            .find(|c: char| c == '=' || c.is_whitespace())
            .unwrap_or(rest.len());
        if key_end == 0 {
            // A stray `=` must not end the attributes after it.
            match rest.strip_prefix('=') {
                Some(after) => {
                    rest = after;
                    continue;
                }
                None => break,
            }
        }
        let key = rest[..key_end].to_ascii_lowercase();
        rest = rest[key_end..].trim_start();
        let Some(after) = rest.strip_prefix('=') else {
            attrs.push((key, String::new()));
            continue;
        };
        let after = after.trim_start();
        let (value, tail) = match after.chars().next() {
            Some(open @ ('"' | '\'')) => {
                let inner = &after[1..];
                let close = inner.find(open).unwrap_or(inner.len());
                (&inner[..close], inner.get(close + 1..).unwrap_or(""))
            }
            _ => {
                let end = after.find(char::is_whitespace).unwrap_or(after.len());
                (&after[..end], &after[end..])
            }
        };
        attrs.push((key, unescape(value)));
        rest = tail;
    }
    Some(Tag {
        name,
        closing,
        self_closing,
        attrs,
    })
}

/// Named entities, and the numeric ones skins break lines with (`&#13;`).
fn unescape(value: &str) -> String {
    if !value.contains('&') {
        return value.to_owned();
    }
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let entity = rest
            .find(';')
            .filter(|end| *end <= 10)
            .map(|end| &rest[1..end]);
        let decoded = entity.and_then(|entity| match entity {
            "quot" => Some('"'),
            "apos" => Some('\''),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "amp" => Some('&'),
            _ => {
                let digits = entity.strip_prefix('#')?;
                let code = match digits.strip_prefix(['x', 'X']) {
                    Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                    None => digits.parse().ok()?,
                };
                char::from_u32(code)
            }
        });
        match (decoded, entity) {
            (Some(character), Some(entity)) => {
                out.push(character);
                rest = &rest[entity.len() + 2..];
            }
            _ => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tag_reader_forgives_what_real_skins_do() {
        let found = tags(
            "<?xml version='1.0'?><!-- hi > there --><THEME TITLE=\"A &amp; B\">\
             <VIEW width=300 Height='100'/><BUTTON onClick=\"if (a > b) go()\" checked></THEME>",
        );
        let names: Vec<_> = found.iter().map(|tag| tag.name.as_str()).collect();
        assert_eq!(names, ["theme", "view", "button", "theme"]);
        assert!(found[1].self_closing && found[3].closing);
        let tree = parse(
            "<THEME TITLE=\"A &amp; B\"><VIEW width=300 Height='100'/>\
             <BUTTON onClick=\"if (a > b) go()\" checked></THEME>",
        );
        let theme = &tree.children[0];
        assert_eq!(theme.attr("title"), Some("A & B"));
        assert_eq!(theme.children[0].attr("height"), Some("100"));
        assert_eq!(theme.children[1].attr("onclick"), Some("if (a > b) go()"));
        assert_eq!(theme.children[1].attr("checked"), Some(""));
    }

    #[test]
    fn an_attribute_given_twice_keeps_the_last() {
        let tree = parse(r#"<subview left="42" top="145" left="11" top="0"/>"#);
        assert_eq!(tree.children[0].attr("left"), Some("11"));
        assert_eq!(tree.children[0].attr("top"), Some("0"));
    }

    #[test]
    fn attributes_need_no_space_between_them() {
        let tree = parse(r#"<a cursor="hand"onClick="go()" bad=1&2 />"#);
        assert_eq!(tree.children[0].attr("cursor"), Some("hand"));
        assert_eq!(tree.children[0].attr("onclick"), Some("go()"));
        assert_eq!(tree.children[0].attr("bad"), Some("1&2"));
    }

    #[test]
    fn line_breaks_and_stray_ampersands_survive() {
        let tree = parse(r#"<text value="a&#13;b &amp; c & d &#xZZ;"/>"#);
        assert_eq!(tree.children[0].attr("value"), Some("a\rb & c & d &#xZZ;"));
    }

    #[test]
    fn nesting_is_tolerated_and_bounded() {
        let tree = parse(&"<a>".repeat(10_000));
        let mut depth = 0;
        let mut node = &tree;
        while let Some(child) = node.children.first() {
            depth += 1;
            node = child;
        }
        assert!(depth <= MAX_DEPTH, "{depth}");
        // A closing tag with no opening one is ignored.
        let tree = parse("<a></b><c/></a>");
        assert_eq!(tree.children[0].children[0].name, "c");
    }
}
