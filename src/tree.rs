use ratatui::{
    buffer::Buffer,
    crossterm::event::{Event, KeyCode, KeyEvent, MouseButton, MouseEventKind},
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::{block::Title, Block, Borders, Widget},
};

use log::trace;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq)]
pub enum TreeNodeStyle {
    Default,
    Pinned,
    Custom(Style),
}

#[derive(Clone, Debug)]
pub struct SimpleTreeNode {
    pub name: String,
    pub is_dir: bool,
    pub style: TreeNodeStyle,
    pub depth: usize,
    pub parent_idx: Option<usize>,
    pub children: Vec<usize>, // Indices of children
    pub full_path: String,    // Keep this for convenience
}

impl SimpleTreeNode {}

pub struct FileTreeWidget {
    pub nodes: Vec<SimpleTreeNode>,
    pub expanded: HashSet<String>,
    pub selected: Option<usize>,
    pub pinned: HashSet<String>,
    focused: bool,
    on_leaf_click: Option<Arc<dyn Fn(&str, SimpleTreeNode) -> TreeNodeStyle>>,
}

impl FileTreeWidget {
    pub fn new(paths: Vec<String>, pinned: Vec<String>) -> Self {
        let mut nodes = Vec::new();
        let mut path_to_idx = HashMap::new(); // Track path -> index mapping

        // First pass: create all nodes
        for path in paths {
            let parts: Vec<_> = path
                .split('/')
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .collect();

            trace!("PATH: {}, PINNED: {:?}", path, pinned);

            let mut style = if pinned.iter().find(|p| **p == path).is_some() {
                TreeNodeStyle::Pinned
            } else {
                TreeNodeStyle::Default
            };

            let mut current_path = String::new();
            for (depth, part) in parts.iter().enumerate() {
                if !current_path.is_empty() {
                    current_path.push('/');
                }
                current_path.push_str(part);

                if !path_to_idx.contains_key(&current_path) {
                    let is_dir = depth < parts.len() - 1;

                    if is_dir {
                        style = TreeNodeStyle::Default
                    }

                    let node = SimpleTreeNode {
                        name: part.clone(),
                        is_dir,
                        style: style.clone(),
                        depth,
                        parent_idx: None, // Will set in second pass
                        children: Vec::new(),
                        full_path: current_path.clone(),
                    };
                    path_to_idx.insert(current_path.clone(), nodes.len());
                    nodes.push(node);
                }
            }
        }

        // Second pass: link parents and children
        for i in 0..nodes.len() {
            let full_path = nodes[i].full_path.clone();
            if let Some(last_slash) = full_path.rfind('/') {
                let parent_path = &full_path[..last_slash];
                if let Some(&parent_idx) = path_to_idx.get(parent_path) {
                    nodes[i].parent_idx = Some(parent_idx);
                    nodes[parent_idx].children.push(i);
                }
            }
        }

        Self {
            nodes,
            expanded: HashSet::new(),
            selected: None,
            focused: true,
            pinned: pinned.into_iter().collect(),
            on_leaf_click: None,
        }
    }

    pub fn set_leaf_callback<F>(&mut self, callback: F)
    where
        F: Fn(&str, SimpleTreeNode) -> TreeNodeStyle + 'static,
    {
        self.on_leaf_click = Some(Arc::new(callback));
    }

    pub fn pin_files(&mut self, paths: &Vec<String>) {
        for path in paths {
            if let Some(idx) = self
                .nodes
                .iter()
                .position(|node| !node.is_dir && node.full_path == *path)
            {
                self.nodes[idx].style = TreeNodeStyle::Pinned;
            }
        }
    }

    fn visible_lines(&self) -> Vec<(usize, usize)> {
        // (display_index, node_index)
        let mut visible = Vec::new();

        fn add_visible_nodes(
            widget: &FileTreeWidget,
            node_idx: usize,
            visible: &mut Vec<(usize, usize)>,
        ) {
            visible.push((visible.len(), node_idx));

            let node = &widget.nodes[node_idx];
            if node.is_dir && widget.expanded.contains(&node.full_path) {
                for &child_idx in &node.children {
                    add_visible_nodes(widget, child_idx, visible);
                }
            }
        }

        // Start with root nodes
        for (idx, node) in self.nodes.iter().enumerate() {
            if node.parent_idx.is_none() {
                add_visible_nodes(self, idx, &mut visible);
            }
        }

        visible
    }
    pub fn handle_event(&mut self, event: &Event, area: Rect) {
        let visible = self.visible_lines();

        let inner_area = Block::default().borders(Borders::ALL).inner(area);
        match event {
            Event::Mouse(mouse_event) => {
                if let MouseEventKind::Down(MouseButton::Left) = mouse_event.kind {
                    let clicked_index = mouse_event.row.saturating_sub(inner_area.y) as usize;
                    trace!("Clicked index: {}", clicked_index);
                    if clicked_index < visible.len() {
                        self.selected = Some(clicked_index);

                        let (_, node_idx) = visible[clicked_index];
                        let node = &self.nodes[node_idx];
                        trace!(
                            "Clicked line: depth={}, path={}, name={}, is_dir={}, node_index={}",
                            node.depth,
                            node.full_path,
                            node.name,
                            node.is_dir,
                            node_idx
                        );

                        if node.is_dir {
                            if self.expanded.contains(&node.full_path) {
                                self.expanded.remove(&node.full_path);
                            } else {
                                self.expanded.insert(node.full_path.clone());
                            }
                        } else if let Some(callback) = &self.on_leaf_click {
                            let full_path = self.nodes[node_idx].clone().full_path;

                            if self.pinned.contains(&full_path) {
                                self.pinned.remove(&full_path)
                            } else {
                                self.pinned.insert(full_path)
                            };

                            self.nodes[node_idx].style =
                                callback(&node.full_path, self.nodes[node_idx].clone());
                        }
                    }
                }
            }
            Event::Key(KeyEvent {
                code: KeyCode::Up, ..
            }) => {
                if let Some(selected) = self.selected {
                    if selected > 0 {
                        self.selected = Some(selected - 1);
                    }
                } else {
                    self.selected = Some(visible.len() - 1);
                }
            }
            Event::Key(KeyEvent {
                code: KeyCode::Down,
                ..
            }) => {
                if let Some(selected) = self.selected {
                    if selected < visible.len() - 1 {
                        self.selected = Some(selected + 1);
                    }
                } else {
                    self.selected = Some(0);
                }
            }
            Event::Key(KeyEvent {
                code: KeyCode::Enter,
                ..
            }) => {
                if let Some(selected) = self.selected {
                    let (_, node_idx) = visible[selected];
                    let node = &self.nodes[node_idx];
                    if node.is_dir {
                        if self.expanded.contains(&node.full_path) {
                            self.expanded.remove(&node.full_path);
                        } else {
                            self.expanded.insert(node.full_path.clone());
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn descendant_selected(&self, node_idx: usize) -> bool {
        let node = &self.nodes[node_idx];
        node.children.iter().any(|&child_idx| {
            self.nodes[child_idx].style == TreeNodeStyle::Pinned
                || self.descendant_selected(child_idx)
        })
    }
}

impl Widget for &mut FileTreeWidget {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let block = Block::new()
            .title(Title::from(" Pinned File Context ").alignment(ratatui::layout::Alignment::Left))
            .borders(ratatui::widgets::Borders::ALL);

        // Get the inner area of the block for content
        let inner_area = block.inner(area);

        // First render the block (border and title)
        block.render(area, buf);

        // Then render the tree contents in the inner area
        let visible = self.visible_lines();
        for (i, (_, node_idx)) in visible.iter().enumerate() {
            if i as u16 >= inner_area.height {
                break;
            }

            let node = &self.nodes[*node_idx];
            let is_selected = Some(i) == self.selected && self.focused;
            let mut style = match node.style {
                TreeNodeStyle::Default => {
                    if node.is_dir {
                        Style::default().fg(Color::Magenta)
                    } else {
                        Style::default()
                    }
                }
                TreeNodeStyle::Pinned => Style::default().fg(Color::Green),
                TreeNodeStyle::Custom(style) => style.clone(),
            };

            if is_selected {
                style = style.bg(Color::DarkGray);
            }

            let prefix = if node.is_dir {
                if self.expanded.contains(&node.full_path) {
                    if self.descendant_selected(*node_idx) {
                        "▼ "
                    } else {
                        "▽ "
                    }
                } else {
                    if self.descendant_selected(*node_idx) {
                        "▶ "
                    } else {
                        "▷ "
                    }
                }
            } else {
                "  "
            };

            let indent = "│ ".repeat(node.depth);
            let line = Line::from(vec![
                Span::styled(&indent, Style::default()),
                Span::styled(prefix, style),
                Span::styled(&node.name, style),
            ]);
            let mut line_area = inner_area;
            line_area.y += i as u16;
            line.render(line_area, buf);
        }
    }
}
