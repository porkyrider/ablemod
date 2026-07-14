//! Thin path-navigation helpers over `xmltree::Element`, replicating just the subset of
//! Python's `xml.etree.ElementTree` `find`/`findall` path syntax export::als actually uses:
//! `"tag"` (direct child), `"./a/b/c"` (direct-child chain from self), and `".//a/b/c"`
//! (first descendant named `a` anywhere below self, then a direct-child chain from there).

use xmltree::{Element, XMLNode};

pub fn child<'a>(el: &'a Element, name: &str) -> Option<&'a Element> {
    el.children.iter().find_map(|n| match n {
        XMLNode::Element(e) if e.name == name => Some(e),
        _ => None,
    })
}

pub fn child_mut<'a>(el: &'a mut Element, name: &str) -> Option<&'a mut Element> {
    el.children.iter_mut().find_map(|n| match n {
        XMLNode::Element(e) if e.name == name => Some(e),
        _ => None,
    })
}

fn find_descendant<'a>(el: &'a Element, name: &str) -> Option<&'a Element> {
    for node in &el.children {
        if let XMLNode::Element(e) = node {
            if e.name == name {
                return Some(e);
            }
            if let Some(found) = find_descendant(e, name) {
                return Some(found);
            }
        }
    }
    None
}

fn find_descendant_mut<'a>(el: &'a mut Element, name: &str) -> Option<&'a mut Element> {
    // an immutable pass first to find a direct-child match's index, so the mutable borrow
    // below is scoped to a single element access rather than the whole iterator (which would
    // conflict with the recursive pass that follows).
    let direct_idx = el.children.iter().position(|n| matches!(n, XMLNode::Element(e) if e.name == name));
    match direct_idx {
        Some(idx) => match &mut el.children[idx] {
            XMLNode::Element(e) => Some(e),
            _ => unreachable!(),
        },
        None => {
            for node in &mut el.children {
                if let XMLNode::Element(e) = node {
                    if let Some(found) = find_descendant_mut(e, name) {
                        return Some(found);
                    }
                }
            }
            None
        }
    }
}

/// `path` is `"tag"`, `"./a/b/c"`, or `".//a/b/c"`.
pub fn find<'a>(el: &'a Element, path: &str) -> Option<&'a Element> {
    if let Some(rest) = path.strip_prefix(".//") {
        let mut parts = rest.split('/');
        let first = parts.next()?;
        let mut current = find_descendant(el, first)?;
        for part in parts {
            current = child(current, part)?;
        }
        Some(current)
    } else if let Some(rest) = path.strip_prefix("./") {
        let mut current = el;
        for part in rest.split('/') {
            current = child(current, part)?;
        }
        Some(current)
    } else {
        child(el, path)
    }
}

pub fn find_mut<'a>(el: &'a mut Element, path: &str) -> Option<&'a mut Element> {
    if let Some(rest) = path.strip_prefix(".//") {
        let mut parts = rest.split('/');
        let first = parts.next()?;
        let mut current = find_descendant_mut(el, first)?;
        for part in parts {
            current = child_mut(current, part)?;
        }
        Some(current)
    } else if let Some(rest) = path.strip_prefix("./") {
        let mut current = el;
        for part in rest.split('/') {
            current = child_mut(current, part)?;
        }
        Some(current)
    } else {
        child_mut(el, path)
    }
}

/// Direct children matching `name` (Python's `el.findall("Name")`).
pub fn find_all_children<'a>(el: &'a Element, name: &str) -> Vec<&'a Element> {
    el.children
        .iter()
        .filter_map(|n| match n {
            XMLNode::Element(e) if e.name == name => Some(e),
            _ => None,
        })
        .collect()
}

fn collect_descendants<'a>(el: &'a Element, name: &str, out: &mut Vec<&'a Element>) {
    for node in &el.children {
        if let XMLNode::Element(e) = node {
            if e.name == name {
                out.push(e);
            }
            collect_descendants(e, name, out);
        }
    }
}

/// All descendants (any depth) matching `name` (Python's `el.findall(".//Name")`).
pub fn find_all_descendants<'a>(el: &'a Element, name: &str) -> Vec<&'a Element> {
    let mut out = Vec::new();
    collect_descendants(el, name, &mut out);
    out
}

/// Sets the `Value` attribute of the element found at `path`, panicking (matching the
/// Python original's `ValueError`) if the path doesn't resolve — a malformed/incompatible
/// template is a hard error, not something to silently limp past.
pub fn set_value(el: &mut Element, path: &str, value: &str) {
    let target = find_mut(el, path).unwrap_or_else(|| panic!("expected element {path:?} not found in template track"));
    target.attributes.insert("Value".to_string(), value.to_string());
}

/// Recursively visits `el` and every descendant (matching Python's `Element.iter()`,
/// which includes the element itself).
pub fn iter_elements(el: &Element) -> Vec<&Element> {
    let mut out = vec![el];
    for node in &el.children {
        if let XMLNode::Element(e) = node {
            out.extend(iter_elements(e));
        }
    }
    out
}

/// Visits `el` and every descendant (matching Python's `Element.iter()`, which includes the
/// element itself), calling `f` on each. Structured as a callback rather than returning
/// `Vec<&mut Element>` since Rust can't express "many disjoint mutable borrows from one
/// tree" any other way without unsafe code.
pub fn visit_mut(el: &mut Element, f: &mut impl FnMut(&mut Element)) {
    f(el);
    for node in &mut el.children {
        if let XMLNode::Element(e) = node {
            visit_mut(e, f);
        }
    }
}
