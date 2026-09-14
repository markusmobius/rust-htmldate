use html5ever::{parse_document, tendril::TendrilSink, tokenizer::TokenizerOpts, ParseOpts};
use markup5ever_rcdom::{NodeData, RcDom};
use std::sync::Arc;

pub(crate) fn repair_html(mut content: String) -> String {
    static PATTERNS: std::sync::OnceLock<[regex::Regex; 2]> = std::sync::OnceLock::new();
    let patterns = PATTERNS.get_or_init(|| {
        [
            regex::Regex::new(r"(?i)^< ?! ?DOCTYPE.+?/ ?>").unwrap(),
            regex::Regex::new(r"(?i)(<html.*?)\s*/>").unwrap(),
        ]
    });
    if crate::rules::limit(&content, 50)
        .to_lowercase()
        .contains("doctype")
    {
        let (first, rest) = content.split_once('\n').unwrap_or((&content, ""));
        content = format!("{}\n{rest}", patterns[0].replace_all(first, ""));
    }
    if content
        .split('\n')
        .take(4)
        .any(|line| line.contains("<html") && line.ends_with("/>"))
    {
        content = patterns[1].replacen(&content, 1, "${1}>").into_owned();
    }
    content
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Document {
    pub(crate) nodes: Arc<[Node]>,
    removed: Vec<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Document,
    Element,
    Text,
    Comment,
    Doctype,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Node {
    pub kind: Kind,
    pub tag: String,
    pub data: String,
    pub attrs: Vec<(String, String)>,
    pub children: Vec<usize>,
    pub parent: Option<usize>,
}

impl Node {
    pub fn attr(&self, key: &str) -> &str {
        self.attrs
            .iter()
            .find(|(name, _)| name == key)
            .map_or("", |(_, value)| value)
    }
}

impl Document {
    pub fn parse(html: &str) -> Self {
        let options = ParseOpts {
            tokenizer: TokenizerOpts {
                discard_bom: false,
                ..TokenizerOpts::default()
            },
            ..ParseOpts::default()
        };
        let parsed = parse_document(RcDom::default(), options).one(html);
        let mut nodes: Vec<Node> = Vec::new();
        let mut pending = vec![(parsed.document.clone(), None)];
        while let Some((handle, parent)) = pending.pop() {
            let mut node = Node {
                kind: Kind::Document,
                tag: String::new(),
                data: String::new(),
                attrs: Vec::new(),
                children: Vec::new(),
                parent,
            };
            let mut children = handle.children.borrow().clone();
            match &handle.data {
                NodeData::Document => {}
                NodeData::Element {
                    name,
                    attrs,
                    template_contents,
                    ..
                } => {
                    node.kind = Kind::Element;
                    node.tag = name.local.to_string();
                    node.attrs = attrs
                        .borrow()
                        .iter()
                        .map(|attribute| {
                            let name = attribute.name.prefix.as_ref().map_or_else(
                                || attribute.name.local.to_string(),
                                |prefix| format!("{prefix}:{}", attribute.name.local),
                            );
                            (name, attribute.value.to_string())
                        })
                        .collect();
                    if name.ns.as_ref() == "http://www.w3.org/1999/xhtml"
                        && matches!(
                            node.tag.as_str(),
                            "a" | "b"
                                | "big"
                                | "code"
                                | "em"
                                | "font"
                                | "i"
                                | "nobr"
                                | "s"
                                | "small"
                                | "strike"
                                | "strong"
                                | "tt"
                                | "u"
                        )
                    {
                        node.attrs.sort_unstable();
                    }
                    if let Some(template) = template_contents.borrow().as_ref() {
                        children.extend(template.children.borrow().iter().cloned());
                    }
                }
                NodeData::Text { contents } => {
                    node.kind = Kind::Text;
                    node.data = contents.borrow().to_string();
                }
                NodeData::Comment { contents } => {
                    node.kind = Kind::Comment;
                    node.data = contents.to_string();
                }
                NodeData::Doctype { name, .. } => {
                    node.kind = Kind::Doctype;
                    node.data = name.to_string();
                }
                NodeData::ProcessingInstruction { target, contents } => {
                    node.kind = Kind::Comment;
                    node.data = format!("?{target} {contents}?");
                }
            }
            let index = nodes.len();
            nodes.push(node);
            if let Some(parent) = parent {
                nodes[parent].children.push(index);
            }
            pending.extend(children.into_iter().rev().map(|child| (child, Some(index))));
        }
        Self {
            removed: vec![false; nodes.len()],
            nodes: nodes.into(),
        }
    }

    pub(crate) fn elements(&self) -> impl Iterator<Item = usize> + '_ {
        self.nodes
            .iter()
            .enumerate()
            .filter(|(index, node)| !self.removed[*index] && node.kind == Kind::Element)
            .map(|(index, _)| index)
    }

    pub(crate) fn tagged(&self, tag: &str) -> Vec<usize> {
        self.elements()
            .filter(|index| self.nodes[*index].tag == tag)
            .collect()
    }

    pub(crate) fn is_removed(&self, index: usize) -> bool {
        self.removed[index]
    }

    pub(crate) fn text(&self, index: usize) -> String {
        let mut result = String::new();
        let mut pending = vec![index];
        while let Some(index) = pending.pop() {
            let node = &self.nodes[index];
            if self.removed[index] {
                continue;
            }
            if node.kind == Kind::Text {
                result.push_str(&node.data);
            }
            pending.extend(node.children.iter().rev());
        }
        result
    }

    pub(crate) fn initial_text(&self, index: usize) -> String {
        let mut result = String::new();
        for child in &self.nodes[index].children {
            let node = &self.nodes[*child];
            if self.removed[*child] {
                continue;
            }
            if node.kind == Kind::Element {
                break;
            }
            if node.kind == Kind::Text {
                result.push_str(&node.data);
            }
        }
        result
    }

    pub(crate) fn remove(&mut self, index: usize) {
        let mut pending = vec![index];
        while let Some(index) = pending.pop() {
            self.removed[index] = true;
            pending.extend(self.nodes[index].children.iter());
        }
    }

    pub fn to_html(&self) -> String {
        self.inner_html(0)
    }

    pub(crate) fn inner_html(&self, index: usize) -> String {
        self.serialize(
            self.nodes[index]
                .children
                .iter()
                .rev()
                .map(|index| (*index, false))
                .collect(),
        )
    }

    pub(crate) fn outer_html(&self, index: usize) -> String {
        self.serialize(vec![(index, false)])
    }

    fn serialize(&self, mut pending: Vec<(usize, bool)>) -> String {
        let mut output = String::new();
        while let Some((index, closing)) = pending.pop() {
            let node = &self.nodes[index];
            if self.removed[index] {
                continue;
            }
            if closing {
                output.push_str("</");
                output.push_str(&node.tag);
                output.push('>');
                continue;
            }
            match node.kind {
                Kind::Element => {
                    output.push('<');
                    output.push_str(&node.tag);
                    for (name, value) in &node.attrs {
                        output.push(' ');
                        output.push_str(name);
                        output.push_str("=\"");
                        escape(value, &mut output);
                        output.push('"');
                    }
                    if matches!(
                        node.tag.as_str(),
                        "area"
                            | "base"
                            | "br"
                            | "col"
                            | "embed"
                            | "hr"
                            | "img"
                            | "input"
                            | "keygen"
                            | "link"
                            | "meta"
                            | "param"
                            | "source"
                            | "track"
                            | "wbr"
                    ) {
                        output.push_str("/>");
                        continue;
                    }
                    output.push('>');
                    if matches!(node.tag.as_str(), "pre" | "listing" | "textarea")
                        && node
                            .children
                            .first()
                            .is_some_and(|child| self.nodes[*child].data.starts_with('\n'))
                    {
                        output.push('\n');
                    }
                    pending.push((index, true));
                }
                Kind::Text => {
                    let raw = node.parent.is_some_and(|parent| {
                        matches!(
                            self.nodes[parent].tag.as_str(),
                            "script"
                                | "style"
                                | "xmp"
                                | "iframe"
                                | "noembed"
                                | "noframes"
                                | "noscript"
                                | "plaintext"
                        )
                    });
                    if raw {
                        output.push_str(&node.data);
                    } else {
                        escape(&node.data, &mut output);
                    }
                }
                Kind::Comment => {
                    output.push_str("<!--");
                    output.push_str(&node.data);
                    output.push_str("-->");
                }
                Kind::Doctype => {
                    output.push_str("<!DOCTYPE ");
                    output.push_str(&node.data);
                    output.push('>');
                }
                Kind::Document => {}
            }
            pending.extend(node.children.iter().rev().map(|child| (*child, false)));
        }
        output.trim().to_owned()
    }
}

fn escape(text: &str, output: &mut String) {
    for character in text.chars() {
        match character {
            '&' => output.push_str("&amp;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            '"' => output.push_str("&#34;"),
            '\'' => output.push_str("&#39;"),
            '\r' => output.push_str("&#13;"),
            _ => output.push(character),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cloned_documents_share_nodes_but_not_pruning() {
        let document = Document::parse(
            "<html><body><div>Discard <span>child</span></div><p>Keep</p></body></html>",
        );
        let original = document.to_html();
        let mut pruned = document.clone();
        assert!(Arc::ptr_eq(&document.nodes, &pruned.nodes));
        pruned.remove(pruned.tagged("div")[0]);
        assert_eq!(document.to_html(), original);
        assert_eq!(document.tagged("span").len(), 1);
        assert!(pruned.tagged("span").is_empty());
        assert!(!pruned.to_html().contains("Discard"));
        assert!(pruned.to_html().contains("Keep"));
        let mut second = pruned.clone();
        second.remove(second.tagged("p")[0]);
        assert!(pruned.to_html().contains("Keep"));
        assert!(!second.to_html().contains("Keep"));
    }

    #[test]
    fn python_html_repairs_and_root_serialization() {
        assert_eq!(repair_html("<!DOCTYPE html><html><head><meta name='date' content='2020-01-01'/><script>date</script></head>".into()), "<script>date</script></head>\n");
        assert_eq!(
            repair_html("<!DOCTYPE html>\n<html lang='en'/>\n<body>text</body>".into()),
            "<!DOCTYPE html>\n<html lang='en'>\n<body>text</body>"
        );
        let document = Document::parse("<html data-archive='2000/01/01'><body>text</body></html>");
        let root = document.tagged("html")[0];
        assert!(document
            .outer_html(root)
            .starts_with("<html data-archive=\"2000/01/01\">"));
        assert!(!document.inner_html(root).contains("data-archive"));
    }

    #[test]
    fn owned_dom_preserves_text_order_and_caller_state() {
        fn shareable<Value: Send + Sync>() {}
        shareable::<Document>();
        let document = Document::parse("<p id='date'>before<iframe>discard</iframe>after<b>bold</b>tail</p><template><time>2020-01-02</time></template>");
        let original = document.clone();
        let mut working = document.clone();
        working.remove(working.tagged("iframe")[0]);
        let paragraph = working.tagged("p")[0];
        assert_eq!(working.nodes[paragraph].attr("id"), "date");
        assert_eq!(working.text(paragraph), "beforeafterboldtail");
        assert_eq!(working.initial_text(paragraph), "beforeafter");
        assert_eq!(working.tagged("time").len(), 1);
        assert_eq!(document, original);
        assert!(working.to_html().contains("beforeafter<b>bold</b>tail"));
    }

    #[test]
    fn go_formatting_attributes_and_bom() {
        let document = Document::parse("<a rel='license' href='/license' class='copyright'>Copyright 2020</a><div rel='license' href='/license' class='copyright'>Copyright 2020</div>");
        assert_eq!(document.to_html(), "<html><head></head><body><a class=\"copyright\" href=\"/license\" rel=\"license\">Copyright 2020</a><div rel=\"license\" href=\"/license\" class=\"copyright\">Copyright 2020</div></body></html>");
        let document = Document::parse(
            "\u{feff}<!--before--><html><body id='news20190624'>content</body></html>",
        );
        assert_eq!(document.to_html(), "<html><head></head><body id=\"news20190624\">\u{feff}<!--before-->content</body></html>");
    }
}
