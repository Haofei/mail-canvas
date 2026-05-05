use anyhow::{Result, bail};
use kuchiki::NodeRef;
use url::Url;

pub(crate) fn document_base_url(document: &NodeRef) -> Option<Url> {
    let base = find_first_tag(document, "base")?;
    let href = attr(&base, "href")?;
    Url::parse(&href).ok()
}

pub(crate) fn ensure_dom_node_limit(document: &NodeRef, max_nodes: usize) -> Result<usize> {
    fn visit(node: &NodeRef, count: &mut usize, max_nodes: usize) -> Result<()> {
        *count = (*count).saturating_add(1);
        if *count > max_nodes {
            let current = *count;
            bail!("document node count exceeds max-dom-nodes: {current} > {max_nodes}");
        }
        for child in node.children() {
            visit(&child, count, max_nodes)?;
        }
        Ok(())
    }

    let mut count = 0usize;
    visit(document, &mut count, max_nodes)?;
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
    if element_tag(node).as_deref() == Some(tag) {
        return Some(node.clone());
    }
    for child in node.children() {
        if let Some(found) = find_first_tag(&child, tag) {
            return Some(found);
        }
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
