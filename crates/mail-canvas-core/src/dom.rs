use anyhow::{Result, bail};
use kuchiki::NodeRef;
use url::Url;

pub(crate) fn document_base_url(document: &NodeRef) -> Option<Url> {
    let base = find_first_tag(document, "base")?;
    let href = attr(&base, "href")?;
    Url::parse(&href).ok()
}

pub(crate) fn ensure_dom_node_limit(document: &NodeRef, max_nodes: usize) -> Result<usize> {
    let mut count = 0usize;
    let mut stack = vec![document.clone()];
    while let Some(node) = stack.pop() {
        count = count.saturating_add(1);
        if count > max_nodes {
            bail!("document node count exceeds max-dom-nodes: {count} > {max_nodes}");
        }
        stack.extend(node.children());
    }
    Ok(count)
}

pub(crate) fn element_tag(node: &NodeRef) -> Option<String> {
    node.as_element()
        .map(|element| element.name.local.to_string())
}

pub(crate) fn is_metadata_tag(tag: &str) -> bool {
    matches!(
        tag,
        "head" | "script" | "style" | "meta" | "link" | "title" | "base"
    )
}

pub(crate) fn find_first_tag(node: &NodeRef, tag: &str) -> Option<NodeRef> {
    let mut stack = vec![node.clone()];
    while let Some(current) = stack.pop() {
        if element_tag(&current).as_deref() == Some(tag) {
            return Some(current);
        }
        let children: Vec<_> = current.children().collect();
        stack.extend(children.into_iter().rev());
    }
    None
}

pub(crate) fn attr(node: &NodeRef, name: &str) -> Option<String> {
    node.as_element().and_then(|element| {
        element
            .attributes
            .borrow()
            .get(name)
            .map(std::borrow::ToOwned::to_owned)
    })
}
