//! Parent/child links between sessions that one OpenCode session spawned.
//!
//! OpenCode's own `parent_id` is not used: it hides a child from the session
//! list and folds its status into the parent. Links live in an append-only
//! file instead, one JSON object per line: `{"child":"ses_..","parent":"ses_.."}`.
//! A later line for the same child replaces an earlier one.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::SystemTime;

use anyhow::{Context, Result};

/// Deepest indentation drawn; deeper rows reuse it so names stay readable.
pub const MAX_VISIBLE_DEPTH: usize = 3;

pub fn parse_links(text: &str) -> HashMap<String, String> {
    let mut links = HashMap::new();
    for line in text.lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let child = value.get("child").and_then(|value| value.as_str());
        let parent = value.get("parent").and_then(|value| value.as_str());
        if let (Some(child), Some(parent)) = (child, parent) {
            if !child.is_empty() && !parent.is_empty() && child != parent {
                links.insert(child.to_owned(), parent.to_owned());
            }
        }
    }
    links
}

/// Child-to-parent map read from disk, re-read only when the file changes.
#[derive(Debug, Default)]
pub struct ParentLinks {
    path: Option<PathBuf>,
    stamp: Option<(SystemTime, u64)>,
    links: HashMap<String, String>,
}

impl ParentLinks {
    pub fn at(path: PathBuf) -> Self {
        Self {
            path: Some(path),
            stamp: None,
            links: HashMap::new(),
        }
    }

    pub fn default_path() -> Result<PathBuf> {
        let relative = std::path::Path::new("agentview/opencode/session-parents.jsonl");
        if let Some(state_home) = crate::fs_util::xdg_home("XDG_STATE_HOME") {
            return Ok(PathBuf::from(state_home).join(relative));
        }
        let home = std::env::var_os("HOME").context("HOME is not set")?;
        Ok(PathBuf::from(home).join(".local/state").join(relative))
    }

    pub fn load_default() -> Result<Self> {
        let mut links = Self::at(Self::default_path()?);
        links.reload_if_changed();
        Ok(links)
    }

    /// Returns true when the links changed since the last call.
    pub fn reload_if_changed(&mut self) -> bool {
        let Some(path) = &self.path else {
            return false;
        };
        let stamp = std::fs::metadata(path)
            .and_then(|meta| Ok((meta.modified()?, meta.len())))
            .ok();
        if stamp == self.stamp {
            return false;
        }
        self.stamp = stamp;
        let links = match stamp {
            Some(_) => std::fs::read_to_string(path)
                .map(|text| parse_links(&text))
                .unwrap_or_default(),
            None => HashMap::new(),
        };
        let changed = links != self.links;
        self.links = links;
        changed
    }

    pub fn links(&self) -> &HashMap<String, String> {
        &self.links
    }
}

/// OpenCode session ids look like `opencode:host:ses_x`; the links file uses
/// the bare `ses_x`.
pub fn raw_opencode_id(session_id: &str) -> Option<&str> {
    let rest = session_id.strip_prefix("opencode:")?;
    rest.split_once(':').map(|(_, id)| id)
}

/// Put each child directly under its parent, depth first, keeping the incoming
/// order among siblings. `parent_of` names the parent position (an index into
/// `items`) when it is in the same group. Returns the items in tree order with
/// their depth. A cycle never loops: unreachable rows become roots.
pub fn arrange(
    items: &[usize],
    parent_of: impl Fn(usize) -> Option<usize>,
) -> (Vec<usize>, Vec<usize>) {
    let positions: HashMap<usize, usize> = items
        .iter()
        .enumerate()
        .map(|(position, item)| (*item, position))
        .collect();
    let mut children: HashMap<usize, Vec<usize>> = HashMap::new();
    let mut roots = Vec::new();
    for item in items {
        match parent_of(*item).filter(|parent| positions.contains_key(parent) && parent != item) {
            Some(parent) => children.entry(parent).or_default().push(*item),
            None => roots.push(*item),
        }
    }
    let mut order = Vec::with_capacity(items.len());
    let mut depths = Vec::with_capacity(items.len());
    let mut seen = HashSet::new();
    fn visit(
        item: usize,
        depth: usize,
        children: &HashMap<usize, Vec<usize>>,
        seen: &mut HashSet<usize>,
        order: &mut Vec<usize>,
        depths: &mut Vec<usize>,
    ) {
        if !seen.insert(item) {
            return;
        }
        order.push(item);
        depths.push(depth);
        for child in children.get(&item).into_iter().flatten() {
            visit(*child, depth + 1, children, seen, order, depths);
        }
    }
    for root in &roots {
        visit(*root, 0, &children, &mut seen, &mut order, &mut depths);
    }
    // Rows that only point at each other have no root; show them flat.
    for item in items {
        visit(*item, 0, &children, &mut seen, &mut order, &mut depths);
    }
    (order, depths)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_links_and_lets_a_later_line_win() {
        let links = parse_links(
            "{\"child\":\"b\",\"parent\":\"a\"}\nnot json\n{\"child\":\"c\",\"parent\":\"a\"}\n{\"child\":\"b\",\"parent\":\"z\"}\n{\"child\":\"x\",\"parent\":\"x\"}\n",
        );
        assert_eq!(links.get("b").map(String::as_str), Some("z"));
        assert_eq!(links.get("c").map(String::as_str), Some("a"));
        assert!(!links.contains_key("x"));
    }

    #[test]
    fn rereads_the_file_only_when_it_changes() {
        let directory = crate::test_support::tempfile::tempdir().unwrap();
        let path = directory
            .path()
            .join("opencode")
            .join("session-parents.jsonl");
        let mut links = ParentLinks::at(path.clone());
        assert!(!links.reload_if_changed());

        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{\"child\":\"b\",\"parent\":\"a\"}\n").unwrap();
        assert!(links.reload_if_changed());
        assert_eq!(links.links().len(), 1);
        assert!(!links.reload_if_changed());

        std::fs::write(
            &path,
            "{\"child\":\"b\",\"parent\":\"a\"}\n{\"child\":\"c\",\"parent\":\"b\"}\n",
        )
        .unwrap();
        assert!(links.reload_if_changed());
        assert_eq!(links.links().len(), 2);
    }

    #[test]
    fn raw_ids_drop_the_provider_and_runtime_prefix() {
        assert_eq!(raw_opencode_id("opencode:host:ses_1"), Some("ses_1"));
        assert_eq!(raw_opencode_id("codex:host:abc"), None);
    }

    #[test]
    fn children_follow_their_parent_depth_first() {
        // items by index: 0 root, 1 child of 3, 2 root, 3 child of 0, 4 child of 1
        let parents = HashMap::from([(1, 3), (3, 0), (4, 1)]);
        let (order, depths) = arrange(&[0, 1, 2, 3, 4], |item| parents.get(&item).copied());
        assert_eq!(order, vec![0, 3, 1, 4, 2]);
        assert_eq!(depths, vec![0, 1, 2, 3, 0]);
    }

    #[test]
    fn a_parent_outside_the_group_leaves_the_child_a_normal_row() {
        let (order, depths) = arrange(&[5, 6], |item| (item == 6).then_some(99));
        assert_eq!(order, vec![5, 6]);
        assert_eq!(depths, vec![0, 0]);
    }

    #[test]
    fn cycles_do_not_loop_or_drop_rows() {
        let parents = HashMap::from([(0, 1), (1, 0)]);
        let (order, depths) = arrange(&[0, 1], |item| parents.get(&item).copied());
        assert_eq!(order.len(), 2);
        assert_eq!(depths.len(), 2);
    }
}
