//! 会话树查询（PLAN §6.5 v1）：由 `session.created.parent_session_id` 派生。
//!
//! 本模块是**纯计算**：输入是 [`SessionMeta`]（目录索引结果）或 [`SessionNode`]，
//! 输出 `children` / `descendants` / `depth` / 根集合，并做无环与深度上限校验。
//! 运行态（active、中断传播、drain 等待）在 `dshr-agent-core` 的会话树运行时里，
//! 不放进存储层。
//!
//! 约定：
//! - 深度从 **0** 开始：根 0、子 1、孙 2；PLAN §6.5 的"默认深度上限 2"即最多两层。
//! - 父会话不在目录里的"孤儿"按根展示（`parent_session_id` 仍保留），不丢弃。
//! - 同层排序按 `updated_at` 倒序，再按 id 升序，保证 UI 输出确定。

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use thiserror::Error;

use crate::{SessionMeta, SpawnSource};

/// 默认深度上限：根 0 → 子 1 → 孙 2（PLAN §6.5）。
pub const DEFAULT_MAX_DEPTH: u32 = 2;

/// 树查询用的节点快照。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionNode {
    pub id: String,
    pub parent_session_id: Option<String>,
    pub spawn_source: Option<SpawnSource>,
    pub title: String,
    /// 同层排序用（索引扫描填文件 mtime；测试可为 0）。
    pub updated_at: u64,
}

impl SessionNode {
    pub fn root(id: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            parent_session_id: None,
            spawn_source: None,
            title: title.into(),
            updated_at: 0,
        }
    }

    pub fn child(
        id: impl Into<String>,
        parent_session_id: impl Into<String>,
        title: impl Into<String>,
    ) -> Self {
        Self {
            parent_session_id: Some(parent_session_id.into()),
            ..Self::root(id, title)
        }
    }

    pub fn with_updated_at(mut self, updated_at: u64) -> Self {
        self.updated_at = updated_at;
        self
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum TreeError {
    #[error("会话 `{session_id}` 在树里出现了两次")]
    Duplicate { session_id: String },
    #[error("会话 `{session_id}` 的父链接成环（父 `{parent_session_id}`）")]
    Cycle {
        session_id: String,
        parent_session_id: String,
    },
    #[error("会话 `{session_id}` 深度 {depth} 超过上限 {limit}")]
    TooDeep {
        session_id: String,
        depth: u32,
        limit: u32,
    },
    #[error("父会话 `{parent_session_id}` 下再建子会话将到达深度 {depth}，超过上限 {limit}")]
    ChildTooDeep {
        parent_session_id: String,
        depth: u32,
        limit: u32,
    },
    #[error("会话 `{session_id}` 不存在")]
    UnknownSession { session_id: String },
}

/// 由父链接派生出的静态会话树。构造时校验重复 ID 与环。
#[derive(Debug, Clone, Default)]
pub struct SessionTree {
    nodes: BTreeMap<String, SessionNode>,
    /// 父 id → 子 id（已排序）。
    children: BTreeMap<String, Vec<String>>,
    roots: Vec<String>,
    depths: BTreeMap<String, u32>,
}

impl SessionTree {
    /// 从目录索引结果构造树。
    pub fn from_metas(metas: &[SessionMeta]) -> Result<Self, TreeError> {
        let nodes = metas.iter().map(|meta| SessionNode {
            id: meta.id.clone(),
            parent_session_id: meta.parent_session_id.clone(),
            spawn_source: meta.spawn_source,
            title: meta.title.clone(),
            updated_at: meta.updated_at,
        });
        Self::from_nodes(nodes)
    }

    /// 从节点集合构造树；重复 ID / 成环返回错误。
    pub fn from_nodes(nodes: impl IntoIterator<Item = SessionNode>) -> Result<Self, TreeError> {
        let mut map = BTreeMap::new();
        for node in nodes {
            let id = node.id.clone();
            if map.insert(id.clone(), node).is_some() {
                return Err(TreeError::Duplicate { session_id: id });
            }
        }

        // 无环校验：每个节点沿父链向上走，重复访问即环。
        for id in map.keys() {
            let mut seen = BTreeSet::new();
            let mut cursor = Some(id.clone());
            while let Some(current) = cursor {
                if !seen.insert(current.clone()) {
                    let parent_session_id =
                        map[&current].parent_session_id.clone().unwrap_or_default();
                    return Err(TreeError::Cycle {
                        session_id: current,
                        parent_session_id,
                    });
                }
                cursor = map[&current]
                    .parent_session_id
                    .clone()
                    .filter(|parent| map.contains_key(parent));
            }
        }

        // 父链接 → 子列表；父不存在的节点按根处理（孤儿）。
        let mut children: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut roots = Vec::new();
        for node in map.values() {
            match &node.parent_session_id {
                Some(parent) if map.contains_key(parent) => {
                    children
                        .entry(parent.clone())
                        .or_default()
                        .push(node.id.clone());
                }
                _ => roots.push(node.id.clone()),
            }
        }

        let sort_key = |left: &String, right: &String| {
            let (left, right) = (&map[left], &map[right]);
            right
                .updated_at
                .cmp(&left.updated_at)
                .then_with(|| left.id.cmp(&right.id))
        };
        for ids in children.values_mut() {
            ids.sort_by(sort_key);
        }
        roots.sort_by(sort_key);

        // 深度：从根 BFS。父缺失的孤儿也是 0。
        let mut depths = BTreeMap::new();
        let mut queue: VecDeque<String> = VecDeque::new();
        for id in &roots {
            depths.insert(id.clone(), 0);
            queue.push_back(id.clone());
        }
        while let Some(id) = queue.pop_front() {
            let depth = depths[&id];
            for child in children.get(&id).into_iter().flatten() {
                depths.insert(child.clone(), depth + 1);
                queue.push_back(child.clone());
            }
        }

        Ok(Self {
            nodes: map,
            children,
            roots,
            depths,
        })
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    pub fn contains(&self, id: &str) -> bool {
        self.nodes.contains_key(id)
    }

    pub fn get(&self, id: &str) -> Option<&SessionNode> {
        self.nodes.get(id)
    }

    pub fn roots(&self) -> Vec<&SessionNode> {
        self.roots.iter().map(|id| &self.nodes[id]).collect()
    }

    /// 直接子级（同层按 `updated_at` 倒序）。
    pub fn children(&self, id: &str) -> Vec<&SessionNode> {
        self.children
            .get(id)
            .into_iter()
            .flatten()
            .map(|child| &self.nodes[child])
            .collect()
    }

    /// 全部后代（不含自己），层序（BFS）：父一定在子前面。
    pub fn descendants(&self, id: &str) -> Vec<&SessionNode> {
        self.descendants_with_depth(id)
            .into_iter()
            .map(|(_, node)| node)
            .collect()
    }

    /// 全部后代及其深度（不含自己）。
    pub fn descendants_with_depth(&self, id: &str) -> Vec<(u32, &SessionNode)> {
        let mut result = Vec::new();
        let mut queue: VecDeque<&str> = VecDeque::new();
        if let Some(children) = self.children.get(id) {
            queue.extend(children.iter().map(String::as_str));
        }
        while let Some(current) = queue.pop_front() {
            let Some(node) = self.nodes.get(current) else {
                continue;
            };
            let depth = self.depths.get(current).copied().unwrap_or(0);
            result.push((depth, node));
            if let Some(children) = self.children.get(current) {
                queue.extend(children.iter().map(String::as_str));
            }
        }
        result
    }

    /// 深度：根 0；未知会话 None。父缺失的孤儿按根 0。
    pub fn depth(&self, id: &str) -> Option<u32> {
        self.depths.get(id).copied()
    }

    /// 父节点；根或父缺失时 None。
    pub fn parent(&self, id: &str) -> Option<&SessionNode> {
        self.nodes
            .get(id)
            .and_then(|node| node.parent_session_id.as_deref())
            .and_then(|parent| self.nodes.get(parent))
    }

    pub fn is_ancestor(&self, ancestor_id: &str, session_id: &str) -> bool {
        let mut cursor = self.nodes.get(session_id);
        let mut steps = 0usize;
        while let Some(node) = cursor {
            // 防御成环数据（构造阶段已拒绝，这里只是不无限循环）。
            steps += 1;
            if steps > self.nodes.len() + 1 {
                return false;
            }
            let Some(parent_id) = node.parent_session_id.as_deref() else {
                return false;
            };
            if parent_id == ancestor_id {
                return true;
            }
            cursor = self.nodes.get(parent_id);
        }
        false
    }

    /// 最深节点的深度（空树 0）。
    pub fn max_depth(&self) -> u32 {
        self.depths.values().copied().max().unwrap_or(0)
    }

    /// 最深的节点（供 `validate_max_depth` 报错定位）。
    pub fn deepest(&self) -> Option<&SessionNode> {
        let (id, _) = self
            .depths
            .iter()
            .max_by(|(left_id, left), (right_id, right)| {
                left.cmp(right).then_with(|| right_id.cmp(left_id))
            })?;
        self.nodes.get(id)
    }

    /// 整棵树的**前序**扁平化（UI 树形 Dock / 侧栏缩进用）：父紧跟着自己的子树，
    /// 然后再轮到兄弟节点。
    pub fn flatten(&self) -> Vec<(u32, &SessionNode)> {
        let mut result = Vec::new();
        for root in &self.roots {
            self.flatten_into(root, 0, &mut result);
        }
        result
    }

    fn flatten_into<'a>(&'a self, id: &str, depth: u32, out: &mut Vec<(u32, &'a SessionNode)>) {
        let Some(node) = self.nodes.get(id) else {
            return;
        };
        out.push((depth, node));
        if let Some(children) = self.children.get(id) {
            for child in children {
                self.flatten_into(child, depth + 1, out);
            }
        }
    }

    /// 校验整棵树的深度不超过上限。
    pub fn validate_max_depth(&self, limit: u32) -> Result<(), TreeError> {
        for (id, depth) in &self.depths {
            if *depth > limit {
                return Err(TreeError::TooDeep {
                    session_id: id.clone(),
                    depth: *depth,
                    limit,
                });
            }
        }
        Ok(())
    }

    /// 在 `parent_id` 下再建一个子会话会不会超限；返回新会话的深度。
    pub fn check_child_depth(&self, parent_id: &str, limit: u32) -> Result<u32, TreeError> {
        let parent_depth = self
            .depth(parent_id)
            .ok_or_else(|| TreeError::UnknownSession {
                session_id: parent_id.to_owned(),
            })?;
        let depth = parent_depth + 1;
        if depth > limit {
            return Err(TreeError::ChildTooDeep {
                parent_session_id: parent_id.to_owned(),
                depth,
                limit,
            });
        }
        Ok(depth)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree() -> SessionTree {
        // root
        // ├── child-b (mtime 30)
        // │   └── grandchild (mtime 40)
        // └── child-a (mtime 20)
        SessionTree::from_nodes([
            SessionNode::root("root", "根").with_updated_at(10),
            SessionNode::child("child-a", "root", "子 A").with_updated_at(20),
            SessionNode::child("child-b", "root", "子 B").with_updated_at(30),
            SessionNode::child("grandchild", "child-b", "孙").with_updated_at(40),
        ])
        .expect("valid tree")
    }

    #[test]
    fn children_are_sorted_by_updated_at_and_descendants_are_breadth_first() {
        let tree = tree();
        let children: Vec<&str> = tree
            .children("root")
            .iter()
            .map(|n| n.id.as_str())
            .collect();
        assert_eq!(children, vec!["child-b", "child-a"]);
        let descendants: Vec<&str> = tree
            .descendants("root")
            .iter()
            .map(|n| n.id.as_str())
            .collect();
        assert_eq!(descendants, vec!["child-b", "child-a", "grandchild"]);
        assert!(tree.children("grandchild").is_empty());
    }

    #[test]
    fn depth_is_zero_based_and_ancestry_is_reported() {
        let tree = tree();
        assert_eq!(tree.depth("root"), Some(0));
        assert_eq!(tree.depth("child-a"), Some(1));
        assert_eq!(tree.depth("grandchild"), Some(2));
        assert_eq!(tree.depth("nope"), None);
        assert!(tree.is_ancestor("root", "grandchild"));
        assert!(tree.is_ancestor("child-b", "grandchild"));
        assert!(!tree.is_ancestor("child-a", "grandchild"));
        assert!(!tree.is_ancestor("grandchild", "root"));
        assert_eq!(tree.max_depth(), 2);
        assert_eq!(
            tree.deepest().map(|node| node.id.as_str()),
            Some("grandchild")
        );
    }

    #[test]
    fn flatten_puts_each_parent_before_its_children() {
        let tree = tree();
        let flat: Vec<(u32, &str)> = tree
            .flatten()
            .into_iter()
            .map(|(depth, node)| (depth, node.id.as_str()))
            .collect();
        assert_eq!(
            flat,
            vec![
                (0, "root"),
                (1, "child-b"),
                (2, "grandchild"),
                (1, "child-a"),
            ]
        );
    }

    #[test]
    fn rejects_duplicate_and_cycles() {
        let duplicate = SessionTree::from_nodes([
            SessionNode::root("root", "根"),
            SessionNode::root("root", "根2"),
        ]);
        assert_eq!(
            duplicate.unwrap_err(),
            TreeError::Duplicate {
                session_id: "root".to_owned()
            }
        );

        let self_parent = SessionTree::from_nodes([SessionNode::child("a", "a", "自环")]);
        assert!(matches!(self_parent.unwrap_err(), TreeError::Cycle { .. }));

        let cycle = SessionTree::from_nodes([
            SessionNode::child("a", "b", "A"),
            SessionNode::child("b", "a", "B"),
        ]);
        assert!(matches!(cycle.unwrap_err(), TreeError::Cycle { .. }));
    }

    #[test]
    fn missing_parent_is_treated_as_root_but_link_is_kept() {
        let tree =
            SessionTree::from_nodes([SessionNode::child("orphan", "deleted-parent", "孤儿")])
                .expect("orphan is allowed");
        assert_eq!(tree.roots().len(), 1);
        assert_eq!(tree.depth("orphan"), Some(0));
        assert_eq!(
            tree.get("orphan").unwrap().parent_session_id.as_deref(),
            Some("deleted-parent")
        );
    }

    #[test]
    fn depth_limit_validation_reports_the_too_deep_session() {
        let tree = tree();
        assert_eq!(tree.validate_max_depth(2), Ok(()));
        assert_eq!(
            tree.validate_max_depth(1).unwrap_err(),
            TreeError::TooDeep {
                session_id: "grandchild".to_owned(),
                depth: 2,
                limit: 1,
            }
        );
        assert_eq!(tree.check_child_depth("root", 2), Ok(1));
        assert_eq!(tree.check_child_depth("child-b", 2), Ok(2));
        assert_eq!(
            tree.check_child_depth("grandchild", 2).unwrap_err(),
            TreeError::ChildTooDeep {
                parent_session_id: "grandchild".to_owned(),
                depth: 3,
                limit: 2,
            }
        );
        assert_eq!(
            tree.check_child_depth("nope", 2).unwrap_err(),
            TreeError::UnknownSession {
                session_id: "nope".to_owned()
            }
        );
    }

    #[test]
    fn root_and_child_constructors_are_ergonomic() {
        let root = SessionNode::root("s-root", "根");
        assert_eq!(root.parent_session_id, None);
        assert_eq!(root.spawn_source, None);
        let child = SessionNode::child("s-child", "s-root", "子");
        assert_eq!(child.parent_session_id.as_deref(), Some("s-root"));
    }
}
