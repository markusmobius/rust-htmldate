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
pub struct Document<Text = String> {
    pub(crate) nodes: Arc<[Node<Text>]>,
    removed: Vec<bool>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TreeAttribute {
    pub namespace: String,
    pub name: String,
    pub value: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TreeNode {
    Document(Vec<TreeNode>),
    Element {
        tag: String,
        attributes: Vec<TreeAttribute>,
        children: Vec<TreeNode>,
    },
    Text(String),
    Comment(String),
    Doctype(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TreeAttributeRef<'tree> {
    pub namespace: &'tree str,
    pub name: &'tree str,
    pub value: &'tree str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TreeNodeRef<'tree> {
    Document,
    Element(&'tree str),
    Text(&'tree str),
    Comment(&'tree str),
    Doctype(&'tree str),
}

pub trait TreeSource {
    type Handle: Copy;

    fn root(&self) -> Self::Handle;
    fn node(&self, node: Self::Handle) -> TreeNodeRef<'_>;
    fn attributes(&self, node: Self::Handle) -> impl Iterator<Item = TreeAttributeRef<'_>>;
    fn children(&self, node: Self::Handle) -> impl DoubleEndedIterator<Item = Self::Handle>;
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
pub(crate) struct Node<Text = String> {
    pub kind: Kind,
    pub tag: Text,
    pub data: Text,
    pub attrs: Vec<(Text, Text)>,
    attr_namespaces: Vec<Text>,
    pub children: Vec<usize>,
    pub parent: Option<usize>,
}

impl<Text: AsRef<str>> Node<Text> {
    pub fn attr(&self, key: &str) -> &str {
        self.attrs
            .iter()
            .find(|(name, _)| name.as_ref() == key)
            .map_or("", |(_, value)| value.as_ref())
    }
}

impl Document {
    pub fn from_tree_source(source: &impl TreeSource) -> Self {
        Self::import_source(source, str::to_owned)
    }

    pub fn from_tree(root: TreeNode) -> Self {
        let root = match root {
            TreeNode::Document(_) => root,
            _ => TreeNode::Document(vec![root]),
        };
        let mut nodes: Vec<Node> = Vec::new();
        let mut pending = vec![(root, None)];
        while let Some((source, parent)) = pending.pop() {
            let mut node = Node {
                kind: Kind::Document,
                tag: String::new(),
                data: String::new(),
                attrs: Vec::new(),
                attr_namespaces: Vec::new(),
                children: Vec::new(),
                parent,
            };
            let children = match source {
                TreeNode::Document(children) => children,
                TreeNode::Element {
                    tag,
                    attributes,
                    children,
                } => {
                    node.kind = Kind::Element;
                    node.tag = tag;
                    let namespaced = attributes
                        .iter()
                        .any(|attribute| !attribute.namespace.is_empty());
                    for attribute in attributes {
                        if namespaced {
                            node.attr_namespaces.push(attribute.namespace);
                        }
                        node.attrs.push((attribute.name, attribute.value));
                    }
                    children
                }
                TreeNode::Text(data) => {
                    node.kind = Kind::Text;
                    node.data = data;
                    Vec::new()
                }
                TreeNode::Comment(data) => {
                    node.kind = Kind::Comment;
                    node.data = data;
                    Vec::new()
                }
                TreeNode::Doctype(data) => {
                    node.kind = Kind::Doctype;
                    node.data = data;
                    Vec::new()
                }
            };
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
                attr_namespaces: Vec::new(),
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
}

impl<Text: AsRef<str>> Document<Text> {
    pub(crate) fn import_source<'tree>(
        source: &'tree impl TreeSource,
        text: impl Fn(&'tree str) -> Text,
    ) -> Self {
        let root = source.root();
        let mut nodes: Vec<Node<Text>> = Vec::new();
        let parent = if matches!(source.node(root), TreeNodeRef::Document) {
            None
        } else {
            nodes.push(Node {
                kind: Kind::Document,
                tag: text(""),
                data: text(""),
                attrs: Vec::new(),
                attr_namespaces: Vec::new(),
                children: Vec::new(),
                parent: None,
            });
            Some(0)
        };
        let mut pending = vec![(root, parent)];
        while let Some((handle, parent)) = pending.pop() {
            let (kind, tag, data, has_children) = match source.node(handle) {
                TreeNodeRef::Document => (Kind::Document, "", "", true),
                TreeNodeRef::Element(tag) => (Kind::Element, tag, "", true),
                TreeNodeRef::Text(data) => (Kind::Text, "", data, false),
                TreeNodeRef::Comment(data) => (Kind::Comment, "", data, false),
                TreeNodeRef::Doctype(data) => (Kind::Doctype, "", data, false),
            };
            let mut node = Node {
                kind,
                tag: text(tag),
                data: text(data),
                attrs: Vec::new(),
                attr_namespaces: Vec::new(),
                children: Vec::new(),
                parent,
            };
            if matches!(node.kind, Kind::Element) {
                let attributes = source.attributes(handle);
                node.attrs.reserve(attributes.size_hint().0);
                for attribute in attributes {
                    if !attribute.namespace.is_empty() && node.attr_namespaces.is_empty() {
                        node.attr_namespaces
                            .resize_with(node.attrs.len(), || text(""));
                    }
                    if !node.attr_namespaces.is_empty() || !attribute.namespace.is_empty() {
                        node.attr_namespaces.push(text(attribute.namespace));
                    }
                    node.attrs
                        .push((text(attribute.name), text(attribute.value)));
                }
            }
            let index = nodes.len();
            nodes.push(node);
            if let Some(parent) = parent {
                nodes[parent].children.push(index);
            }
            if has_children {
                let children = source.children(handle);
                nodes[index].children.reserve(children.size_hint().0);
                pending.extend(children.rev().map(|child| (child, Some(index))));
            }
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
            .filter(|index| self.nodes[*index].tag.as_ref() == tag)
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
                result.push_str(node.data.as_ref());
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
                result.push_str(node.data.as_ref());
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
                output.push_str(node.tag.as_ref());
                output.push('>');
                continue;
            }
            match node.kind {
                Kind::Element => {
                    output.push('<');
                    output.push_str(node.tag.as_ref());
                    for (index, (name, value)) in node.attrs.iter().enumerate() {
                        output.push(' ');
                        if let Some(namespace) = node
                            .attr_namespaces
                            .get(index)
                            .filter(|namespace| !namespace.as_ref().is_empty())
                        {
                            output.push_str(namespace.as_ref());
                            output.push(':');
                        }
                        output.push_str(name.as_ref());
                        output.push_str("=\"");
                        escape(value.as_ref(), &mut output);
                        output.push('"');
                    }
                    if matches!(
                        node.tag.as_ref(),
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
                    if matches!(node.tag.as_ref(), "pre" | "listing" | "textarea")
                        && node
                            .children
                            .first()
                            .is_some_and(|child| self.nodes[*child].data.as_ref().starts_with('\n'))
                    {
                        output.push('\n');
                    }
                    pending.push((index, true));
                }
                Kind::Text => {
                    let raw = node.parent.is_some_and(|parent| {
                        matches!(
                            self.nodes[parent].tag.as_ref(),
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
                        output.push_str(node.data.as_ref());
                    } else {
                        escape(node.data.as_ref(), &mut output);
                    }
                }
                Kind::Comment => {
                    output.push_str("<!--");
                    output.push_str(node.data.as_ref());
                    output.push_str("-->");
                }
                Kind::Doctype => {
                    output.push_str("<!DOCTYPE ");
                    output.push_str(node.data.as_ref());
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

    fn tree_element(
        tag: &str,
        attributes: &[(&str, &str, &str)],
        children: Vec<TreeNode>,
    ) -> TreeNode {
        TreeNode::Element {
            tag: tag.into(),
            attributes: attributes
                .iter()
                .map(|(namespace, name, value)| TreeAttribute {
                    namespace: (*namespace).into(),
                    name: (*name).into(),
                    value: (*value).into(),
                })
                .collect(),
            children,
        }
    }

    #[test]
    fn borrowed_import_matches_owned_tree() {
        struct Source<'tree>(&'tree TreeNode);

        impl<'tree> TreeSource for Source<'tree> {
            type Handle = &'tree TreeNode;

            fn root(&self) -> Self::Handle {
                self.0
            }

            fn node(&self, node: Self::Handle) -> TreeNodeRef<'_> {
                match node {
                    TreeNode::Document(_) => TreeNodeRef::Document,
                    TreeNode::Element { tag, .. } => TreeNodeRef::Element(tag),
                    TreeNode::Text(data) => TreeNodeRef::Text(data),
                    TreeNode::Comment(data) => TreeNodeRef::Comment(data),
                    TreeNode::Doctype(data) => TreeNodeRef::Doctype(data),
                }
            }

            fn attributes(&self, node: Self::Handle) -> impl Iterator<Item = TreeAttributeRef<'_>> {
                let attributes = match node {
                    TreeNode::Element { attributes, .. } => attributes.as_slice(),
                    _ => &[],
                };
                attributes.iter().map(|attribute| TreeAttributeRef {
                    namespace: &attribute.namespace,
                    name: &attribute.name,
                    value: &attribute.value,
                })
            }

            fn children(
                &self,
                node: Self::Handle,
            ) -> impl DoubleEndedIterator<Item = Self::Handle> {
                let children = match node {
                    TreeNode::Document(children) | TreeNode::Element { children, .. } => {
                        children.as_slice()
                    }
                    _ => &[],
                };
                children.iter()
            }
        }

        let element = tree_element(
            "p",
            &[
                ("", "id", "first"),
                ("", "id", "second"),
                ("xlink", "href", "target"),
                ("", "title", "tail"),
            ],
            vec![
                TreeNode::Text("before\r\0".into()),
                tree_element("div", &[("xml", "lang", "en"), ("", "lang", "de")], vec![]),
                TreeNode::Comment("comment".into()),
                tree_element(
                    "meta",
                    &[
                        ("", "name", "date"),
                        ("custom", "content", "2020-01-02"),
                        ("", "content", "2021-03-04"),
                    ],
                    vec![],
                ),
                TreeNode::Text("after".into()),
            ],
        );
        for root in [
            TreeNode::Document(vec![]),
            TreeNode::Document(vec![TreeNode::Doctype("html".into()), element.clone()]),
            element,
            TreeNode::Text("text root".into()),
            TreeNode::Comment("comment root".into()),
            TreeNode::Doctype("doctype root".into()),
        ] {
            let original = root.clone();
            let expected = Document::from_tree(root.clone());
            let actual = Document::from_tree_source(&Source(&root));
            assert_eq!(actual, expected);
            assert_eq!(actual.to_html(), expected.to_html());
            assert_eq!(
                crate::from_document(&actual, &crate::Options::default()),
                crate::from_document(&expected, &crate::Options::default())
            );
            let source = Source(&root);
            let borrowed = Document::import_source(&source, |text| text);
            assert_eq!(borrowed.to_html(), expected.to_html());
            if let TreeNode::Element {
                tag, attributes, ..
            } = &root
            {
                assert_eq!(borrowed.nodes[1].tag.as_ptr(), tag.as_ptr());
                for (actual, expected) in borrowed.nodes[1].attrs.iter().zip(attributes) {
                    assert_eq!(actual.0.as_ptr(), expected.name.as_ptr());
                    assert_eq!(actual.1.as_ptr(), expected.value.as_ptr());
                }
            }
            for use_original_date in [false, true] {
                for skip_extensive_search in [false, true] {
                    let options = crate::Options {
                        use_original_date,
                        skip_extensive_search,
                        ..crate::Options::default()
                    };
                    assert_eq!(
                        crate::from_tree_source(&source, &options),
                        crate::from_document(&expected, &options)
                    );
                }
            }
            assert_eq!(root, original);
        }
    }

    #[test]
    fn import_preserves_topology_attributes_and_text() {
        let document = Document::from_tree(tree_element(
            "p",
            &[],
            vec![
                TreeNode::Text("before\r\0".into()),
                tree_element(
                    "div",
                    &[
                        ("", "id", "first"),
                        ("", "id", "second"),
                        ("xlink", "href", "target"),
                    ],
                    vec![TreeNode::Text("inside".into())],
                ),
                TreeNode::Comment("comment".into()),
                TreeNode::Text("after".into()),
            ],
        ));
        let paragraph = document.tagged("p")[0];
        let division = document.tagged("div")[0];
        assert_eq!(document.nodes[division].parent, Some(paragraph));
        assert_eq!(document.nodes[division].attr("id"), "first");
        assert_eq!(document.nodes[division].attr("href"), "target");
        assert_eq!(document.text(paragraph), "before\r\0insideafter");
        assert!(document
            .to_html()
            .contains("id=\"first\" id=\"second\" xlink:href=\"target\""));
        let mut copy = document.clone();
        copy.remove(division);
        assert!(Arc::ptr_eq(&document.nodes, &copy.nodes));
        assert_eq!(document.text(paragraph), "before\r\0insideafter");
        assert_eq!(copy.text(paragraph), "before\r\0after");
    }

    #[test]
    fn import_extracts_date_and_preserves_caller() {
        let document = Document::from_tree(tree_element(
            "html",
            &[],
            vec![
                tree_element(
                    "head",
                    &[],
                    vec![tree_element(
                        "meta",
                        &[
                            ("", "name", "date"),
                            ("custom", "content", "2020-01-02"),
                            ("", "content", "2021-03-04"),
                        ],
                        vec![],
                    )],
                ),
                tree_element("body", &[], vec![TreeNode::Text("Article".into())]),
            ],
        ));
        let original = document.clone();
        let result = crate::from_document(&document, &crate::Options::default());
        assert_eq!(result.date_time.format("%F").to_string(), "2020-01-02");
        assert_eq!(document, original);
        assert!(document.to_html().contains("custom:content=\"2020-01-02\""));
    }

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
