use std::{
    cell::OnceCell,
    collections::{HashSet, VecDeque},
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{anyhow, Result};
use copypasta::ClipboardProvider;
use derivative::Derivative;
use futures::{
    stream::{SplitSink, SplitStream},
    SinkExt, StreamExt, TryStreamExt,
};
use log::{debug, trace};
use ratatui::{
    crossterm::{
        cursor::SetCursorStyle,
        event::{self, Event, KeyCode, MouseButton},
    },
    layout::{Constraint, Layout, Margin, Rect},
    style::{Style, Stylize},
    text::{Line, Span},
    widgets::{
        Block, Clear, Padding, Paragraph, Scrollbar, ScrollbarState, StatefulWidget, Widget,
    },
};
use serde_json::json;
use syntect::easy::HighlightLines;
use syntect::util::LinesWithEndings;
use tokio::{net::TcpStream, sync::mpsc};
use tokio_tungstenite::{
    connect_async, tungstenite::protocol::Message, MaybeTlsStream, WebSocketStream,
};
use url::Url;

use crate::{
    api::{
        self,
        ws::{ChatModifiedFile, RunCommandResponse},
    },
    APIClient, ResponseErrorExt as _,
};

fn websocket_url(api_url: &Url) -> &'static str {
    match api_url.host_str() {
        Some("localhost") => "ws://localhost:8765",
        Some("api-staging.bismuth.cloud") => "wss://chat-staging.bismuth.cloud",
        _ => "wss://chat.bismuth.cloud",
    }
}

/// List files that have changed in the working directory compared to the upstream branch.
fn list_changed_files(repo_path: &Path) -> Result<Vec<PathBuf>> {
    let repo = git2::Repository::open(&repo_path)?;
    let branch = repo.head()?.shorthand().unwrap().to_string();
    let upstream_commit = repo
        .find_branch(&format!("bismuth/{}", branch), git2::BranchType::Remote)?
        .get()
        .target()
        .unwrap();
    let upstream_tree = repo.find_commit(upstream_commit)?.tree()?;
    let head_tree = repo.find_commit(repo.head()?.target().unwrap())?.tree()?;
    // Diff tree to HEAD
    let diff = repo.diff_tree_to_tree(Some(&upstream_tree), Some(&head_tree), None)?;
    let mut changed_files = HashSet::new();
    diff.foreach(
        &mut |delta, _| {
            changed_files.insert(delta.new_file().path().unwrap().to_path_buf());
            true
        },
        None,
        None,
        None,
    )?;
    // Then index to workdir + untracked
    let statuses = repo.statuses(None)?;
    for status in statuses.iter() {
        match status.status() {
            git2::Status::WT_NEW
            | git2::Status::WT_MODIFIED
            | git2::Status::WT_DELETED
            | git2::Status::INDEX_NEW
            | git2::Status::INDEX_MODIFIED
            | git2::Status::INDEX_DELETED => {
                changed_files.insert(PathBuf::from(status.path().unwrap()));
            }
            git2::Status::WT_RENAMED | git2::Status::INDEX_RENAMED => {
                if let Some(stuff) = status.head_to_index() {
                    changed_files.insert(PathBuf::from(stuff.old_file().path().unwrap()));
                    changed_files.insert(PathBuf::from(stuff.new_file().path().unwrap()));
                }
                if let Some(stuff) = status.index_to_workdir() {
                    changed_files.insert(PathBuf::from(stuff.old_file().path().unwrap()));
                    changed_files.insert(PathBuf::from(stuff.new_file().path().unwrap()));
                }
            }
            _ => {}
        }
    }
    Ok(changed_files.into_iter().collect())
}

fn process_chat_message(
    repo_path: &Path,
    modified_files: &[ChatModifiedFile],
) -> Result<Option<String>> {
    let repo_path = std::fs::canonicalize(repo_path)?;
    let repo = git2::Repository::open(&repo_path)?;

    if modified_files.len() == 0 {
        return Ok(None);
    }

    let mut index = repo.index()?;
    index.add_all(&["*"], git2::IndexAddOption::DEFAULT, None)?;
    index.write()?;
    let tree_id = index.write_tree()?;
    let tree = repo.find_tree(tree_id)?;
    let head = repo.head()?;
    let parent_commit = repo.find_commit(head.target().unwrap())?;
    let signature = git2::Signature::now(
        "bismuthdev[bot]",
        "bismuthdev[bot]@users.noreply.github.com",
    )?;
    repo.commit(
        Some("HEAD"),
        &signature,
        &signature,
        "Bismuth Temp Commit",
        &tree,
        &[&parent_commit],
    )?;

    for mf in modified_files {
        trace!("Writing file: {}", mf.project_path);
        let mut file_name = mf.project_path.as_str();
        file_name = file_name.trim_start_matches('/');
        let full_path = repo_path.join(file_name);
        if !full_path.starts_with(&repo_path) {
            return Err(anyhow::anyhow!("Invalid file path"));
        }
        std::fs::create_dir_all(full_path.parent().unwrap())?;
        std::fs::write(full_path, &mf.content)?;
    }

    index.add_all(&["*"], git2::IndexAddOption::DEFAULT, None)?;
    index.write()?;

    let diff = Command::new("git")
        .arg("-C")
        .arg(&repo_path)
        .arg("--no-pager")
        .arg("diff")
        .arg("--staged")
        .output()
        .map_err(|e| anyhow!("Failed to run git diff: {}", e))
        .and_then(|o| {
            if o.status.success() {
                Ok(o.stdout)
            } else {
                Err(anyhow!("git diff failed (code={})", o.status))
            }
        })
        .and_then(|s| String::from_utf8(s).map_err(|e| anyhow!(e)))?
        .replace("\t", "    ");

    Ok(Some(diff))
}

fn commit(repo_path: &Path, message: Option<&str>) -> Result<()> {
    Command::new("git")
        .arg("-C")
        .arg(&repo_path)
        .arg("reset")
        .arg("HEAD~1")
        .output()
        .map_err(|e| anyhow!("Failed to run git reset: {}", e))
        .and_then(|o| {
            if o.status.success() {
                Ok(o.stdout)
            } else {
                Err(anyhow!("git reset failed (code={})", o.status))
            }
        })
        .and_then(|s| String::from_utf8(s).map_err(|e| anyhow!(e)))?;

    let repo = git2::Repository::open(&repo_path)?;
    let mut index = repo.index()?;
    index.add_all(&["*"], git2::IndexAddOption::DEFAULT, None)?;
    index.write()?;
    let tree_id = index.write_tree()?;
    let tree = repo.find_tree(tree_id)?;

    let head = repo.head()?;
    let parent_commit = repo.find_commit(head.target().unwrap())?;

    let signature = git2::Signature::now(
        "bismuthdev[bot]",
        "bismuthdev[bot]@users.noreply.github.com",
    )?;

    let diff = repo.diff_tree_to_index(Some(&parent_commit.tree()?), Some(&index), None)?;
    let mut changed_files = vec![];
    diff.foreach(
        &mut |delta, _| {
            changed_files.push(
                delta
                    .new_file()
                    .path()
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .to_string(),
            );
            true
        },
        None,
        None,
        None,
    )?;

    let message = match message {
        Some(m) => m.to_string(),
        None => format!("Bismuth: {}", changed_files.join(", ")),
    };

    repo.commit(
        Some("HEAD"),
        &signature,
        &signature,
        &message,
        &tree,
        &[&parent_commit],
    )?;

    Ok(())
}

fn revert(repo_path: &Path) -> Result<()> {
    let repo = git2::Repository::open(&repo_path)?;

    let mut index = repo.index()?;
    index.remove_all(&["*"], None)?;
    index.write()?;

    Command::new("git")
        .arg("-C")
        .arg(&repo_path)
        .arg("reset")
        .arg("--hard")
        .output()
        .map_err(|e| anyhow!("Failed to run git reset: {}", e))
        .and_then(|o| {
            if o.status.success() {
                Ok(())
            } else {
                Err(anyhow!("git reset failed (code={})", o.status))
            }
        })?;

    Command::new("git")
        .arg("-C")
        .arg(&repo_path)
        .arg("clean")
        .arg("-f")
        .arg("-d")
        .output()
        .map_err(|e| anyhow!("Failed to run git clean: {}", e))
        .and_then(|o| {
            if o.status.success() {
                Ok(())
            } else {
                Err(anyhow!("git clean failed (code={})", o.status))
            }
        })?;

    Command::new("git")
        .arg("-C")
        .arg(&repo_path)
        .arg("reset")
        .arg("HEAD~1")
        .output()
        .map_err(|e| anyhow!("Failed to run git reset: {}", e))
        .and_then(|o| {
            if o.status.success() {
                Ok(())
            } else {
                Err(anyhow!("git reset failed (code={})", o.status))
            }
        })?;

    Ok(())
}

#[derive(Clone, Debug)]
enum ChatMessageUser {
    User(String),
    AI,
}

#[derive(Clone, Debug, Derivative)]
#[derivative(PartialEq)]
struct OwnedLine {
    spans: Vec<(String, Style)>,
}

impl From<Vec<(&str, Style)>> for OwnedLine {
    fn from(spans: Vec<(&str, Style)>) -> Self {
        Self {
            spans: spans
                .into_iter()
                .map(|(s, style)| (s.into(), style))
                .collect(),
        }
    }
}

impl From<&str> for OwnedLine {
    fn from(s: &str) -> Self {
        Self {
            spans: vec![(s.into(), Style::default())],
        }
    }
}

impl<'a> OwnedLine {
    fn as_line(&'a self) -> Line<'a> {
        Line::from(
            self.spans
                .iter()
                .map(|(s, style)| Span::styled(s, *style))
                .collect::<Vec<_>>(),
        )
    }
}

#[derive(Clone, Debug, Derivative)]
#[derivative(PartialEq)]
struct CodeBlock {
    filename: Option<String>,
    language: String,
    raw_code: String,

    /// Cached syntax highlighted code
    #[derivative(PartialEq = "ignore")]
    lines: OnceCell<Vec<OwnedLine>>,

    #[derivative(PartialEq = "ignore")]
    folded: bool,
}

impl CodeBlock {
    fn new(filename: Option<&str>, language: Option<&str>, raw_code: &str) -> Self {
        Self {
            filename: filename.map(|f| f.to_string()),
            language: language.unwrap_or("").to_string(),
            raw_code: raw_code.to_string().replace("\t", "    "),
            lines: OnceCell::new(),
            folded: true,
        }
    }
    fn lines(&self) -> &Vec<OwnedLine> {
        self.lines.get_or_init(|| {
            let ps = two_face::syntax::extra_newlines();
            let ts = two_face::theme::extra();
            let syntax = ps
                .syntaxes()
                .iter()
                .find(|s| s.name.to_lowercase() == self.language.to_lowercase())
                .unwrap_or(ps.find_syntax_plain_text());
            let mut h = HighlightLines::new(
                syntax,
                ts.get(two_face::theme::EmbeddedThemeName::Base16OceanDark),
            );
            LinesWithEndings::from(&self.raw_code)
                .map(|line| {
                    OwnedLine::from(
                        h.highlight_line(line, &ps)
                            .unwrap()
                            .into_iter()
                            .map(|(syntect_style, content)| {
                                (
                                    content,
                                    Style {
                                        fg: match syntect_style.foreground {
                                            // TODO: detect terminal and disable highlighting if 24 bit color is unsupported
                                            syntect::highlighting::Color { r, g, b, a } => {
                                                Some(ratatui::style::Color::Rgb(r, g, b))
                                            }
                                        },
                                        bg: None,
                                        underline_color: None,
                                        add_modifier: ratatui::style::Modifier::empty(),
                                        sub_modifier: ratatui::style::Modifier::empty(),
                                    },
                                )
                            })
                            .collect::<Vec<_>>(),
                    )
                })
                .collect()
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
enum MessageBlock {
    Text(Vec<OwnedLine>),
    Thinking(String),
    Code(CodeBlock),
}

impl MessageBlock {
    fn new_text(text: &str) -> Self {
        Self::Text(text.split('\n').map(|line| OwnedLine::from(line)).collect())
    }
}

#[derive(Clone, Debug)]
struct ChatMessage {
    user: ChatMessageUser,
    raw: String,
    finalized: bool,
    blocks: Vec<MessageBlock>,
    block_line_cache: (usize, Vec<usize>),
}

impl ChatMessage {
    pub fn new(user: ChatMessageUser, content: &str) -> Self {
        let content = content
            .replace("\n<BCODE>\n", "\n")
            .replace("\n</BCODE>\n", "\n");
        let mut blocks = Self::parse_md(&content);
        let prefix_spans = Self::format_user(&user);

        if let Some(MessageBlock::Text(text_lines)) = blocks.first_mut() {
            text_lines[0].spans = prefix_spans
                .spans
                .into_iter()
                .chain(text_lines[0].spans.drain(..))
                .collect();
        } else {
            blocks.insert(0, MessageBlock::Text(vec![prefix_spans]));
        }

        Self {
            user,
            raw: content.to_string(),
            finalized: false,
            blocks,
            // Cache the result of line wrapping for each block. This is surprisingly expensive
            block_line_cache: (0, vec![]), // width, list of rendered line counts for each block
        }
    }

    fn format_user(user: &ChatMessageUser) -> OwnedLine {
        let mut spans = Vec::with_capacity(3);
        // Copy
        if copypasta::ClipboardContext::new().is_ok() {
            spans.push(("⎘ ", ratatui::style::Style::default()));
        }
        spans.push(match user {
            ChatMessageUser::AI => (
                "Bismuth",
                ratatui::style::Style::default().fg(ratatui::style::Color::Magenta),
            ),
            ChatMessageUser::User(ref user) => (
                user,
                ratatui::style::Style::default().fg(ratatui::style::Color::Cyan),
            ),
        });
        spans.push((": ".into(), ratatui::style::Style::default()));
        OwnedLine::from(spans)
    }

    fn parse_md(text: &str) -> Vec<MessageBlock> {
        let root = markdown::to_mdast(text, &markdown::ParseOptions::default()).unwrap();
        match root.children() {
            Some(nodes) => nodes
                .into_iter()
                .filter_map(|block| match block {
                    markdown::mdast::Node::Code(code_block) => {
                        if code_block.value.len() > 0 {
                            let fn_line = code_block.value.lines().next().unwrap();
                            let mut filename = None;
                            let mut code = code_block.value.clone();
                            if fn_line.starts_with("FILENAME:") {
                                filename = Some(
                                    fn_line.trim_start_matches("FILENAME:").trim().to_string(),
                                );
                                code = code.lines().skip(1).collect::<Vec<_>>().join("\n");
                            }
                            Some(MessageBlock::Code(CodeBlock::new(
                                filename.as_deref(),
                                code_block.lang.as_deref(),
                                &code,
                            )))
                        } else {
                            None
                        }
                    }
                    _ => {
                        // Slice from content based on position instead of node.to_string()
                        // so that we get things like bullet points, list numbering, etc.
                        let position = block.position().unwrap();
                        Some(MessageBlock::new_text(
                            &text[position.start.offset..position.end.offset],
                        ))
                    }
                })
                .collect(),
            None => vec![],
        }
    }

    pub fn append(&mut self, token: &str) {
        self.raw += token;
        let mut blocks = Self::parse_md(&self.raw);
        let prefix_spans = Self::format_user(&self.user);

        if let Some(MessageBlock::Text(text_lines)) = blocks.first_mut() {
            text_lines[0].spans = prefix_spans
                .spans
                .into_iter()
                .chain(text_lines[0].spans.drain(..))
                .collect();
        } else {
            blocks.insert(0, MessageBlock::Text(vec![prefix_spans]));
        }

        // Update any existing blocks
        for (i, (existing, new)) in self.blocks.iter_mut().zip(blocks.iter()).enumerate() {
            if existing != new {
                *existing = new.clone();
                self.block_line_cache.1.truncate(i);
            }
        }

        // And add any new blocks
        self.blocks
            .extend_from_slice(&blocks[self.blocks.len().min(blocks.len())..]);
    }
}

impl From<api::ChatMessage> for ChatMessage {
    fn from(message: api::ChatMessage) -> Self {
        let mut msg = ChatMessage::new(
            if message.is_ai {
                ChatMessageUser::AI
            } else {
                ChatMessageUser::User(message.user.as_ref().unwrap().name.clone())
            },
            &message.content,
        );
        msg.finalized = true;
        msg
    }
}

fn title_case(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(f) => f.to_uppercase().chain(chars).collect(),
    }
}

struct ChatHistoryWidget {
    messages: Arc<Mutex<Vec<ChatMessage>>>,
    scroll_position: usize,
    scroll_max: usize,
    scroll_state: ratatui::widgets::ScrollbarState,
    code_block_hitboxes: Vec<(usize, usize)>,
    message_hitboxes: Vec<(usize, usize)>,
    sessions: Vec<api::ChatSession>,
    session: api::ChatSession,

    selection: Option<((usize, usize), (usize, usize))>,
}

impl Widget for &mut ChatHistoryWidget {
    fn render(self, area: ratatui::layout::Rect, buf: &mut ratatui::buffer::Buffer) {
        let block = Block::new()
            .title(format!(
                "Chat History{}",
                match &self.session._name {
                    Some(name) => format!(" ({})", name),
                    None => "".to_string(),
                }
            ))
            .borders(ratatui::widgets::Borders::ALL);

        let mut line_idx = 0;

        // start,end line idxs for each code block
        let mut code_block_hitboxes: Vec<(usize, usize)> = vec![];
        let mut message_hitboxes: Vec<(usize, usize)> = vec![];

        let mut messages = self.messages.lock().unwrap();
        if messages.len() > 0 {
            let lines: Vec<_> = messages
                .iter_mut()
                .flat_map(|message| {
                    let mut rendered_line_lens = vec![];
                    let message_lines: Vec<_> = message
                        .blocks
                        .iter()
                        .enumerate()
                        .flat_map(|(idx, block)| {
                            let mut lines = match block {
                                MessageBlock::Text(lines) => {
                                    lines.iter().map(OwnedLine::as_line).collect()
                                }
                                MessageBlock::Thinking(detail) => {
                                    let is_last = idx == message.blocks.len() - 1;
                                    let indicator = if is_last {
                                        vec!['|', '\\', '-', '/'][SystemTime::now()
                                            .duration_since(UNIX_EPOCH)
                                            .unwrap()
                                            .subsec_millis()
                                            as usize
                                            / 251]
                                    } else {
                                        '✓'
                                    };
                                    vec![Line::styled(
                                        format!("  {} {}", detail, indicator),
                                        ratatui::style::Style::default().fg(if is_last {
                                            ratatui::style::Color::LightGreen
                                        } else {
                                            ratatui::style::Color::Green
                                        }),
                                    )]
                                }
                                MessageBlock::Code(code) => {
                                    let code_block_lines = if code.folded {
                                        vec![Line::styled(
                                            if let Some(filename) = &code.filename {
                                                format!("Change to {} (click to expand)", &filename)
                                            } else {
                                                title_case(
                                                    &format!(
                                                        "{} code block (click to expand)",
                                                        &code.language
                                                    )
                                                    .trim(),
                                                )
                                            },
                                            ratatui::style::Style::default()
                                                .fg(ratatui::style::Color::Yellow),
                                        )]
                                    } else {
                                        code.lines()
                                            .iter()
                                            .map(|line| {
                                                let mut indented = line.as_line();
                                                indented.spans.insert(0, "│ ".into());
                                                indented
                                            })
                                            .collect()
                                    };
                                    code_block_hitboxes
                                        .push((line_idx, line_idx + code_block_lines.len()));
                                    code_block_lines
                                }
                            };
                            lines.push(Line::raw(""));
                            let rendered_line_len = if message.finalized
                                && message.block_line_cache.0 == area.width as usize
                                && message.block_line_cache.1.len() > idx
                            {
                                message.block_line_cache.1[idx]
                            } else {
                                // Just resized, so clear the cache.
                                // The len guard above will make sure we end up recalculating each block
                                if message.block_line_cache.0 != area.width as usize {
                                    message.block_line_cache.0 = area.width as usize;
                                    message.block_line_cache.1.clear();
                                }
                                // have to "simulate" line wrapping here to get an accurate line count
                                let res =
                                    Paragraph::new(ratatui::text::Text::from_iter(lines.clone()))
                                        .wrap(ratatui::widgets::Wrap { trim: false })
                                        .line_count(area.width - 2); // -1 for each L/R border
                                message.block_line_cache.1.push(res);
                                res
                            };
                            rendered_line_lens.push(rendered_line_len);
                            line_idx += rendered_line_len;
                            lines
                        })
                        .collect();

                    message_hitboxes.push((
                        line_idx - rendered_line_lens.iter().sum::<usize>(),
                        line_idx,
                    ));
                    message_lines
                })
                .collect();

            let paragraph = Paragraph::new(ratatui::text::Text::from_iter(lines))
                .block(block)
                .scroll((self.scroll_position as u16, 0))
                .wrap(ratatui::widgets::Wrap { trim: false });

            // +3 so you can scroll past the bottom a bit to see this is really the end
            let nlines = message_hitboxes.last().unwrap_or(&(0, 0)).1 + 3;
            let old_scroll_max = self.scroll_max;
            self.scroll_max = nlines.max(area.height as usize) - (area.height as usize);
            // Auto scroll to the bottom if we were already at the bottom
            if self.scroll_position == old_scroll_max {
                self.scroll_position = self.scroll_max;
            }
            self.scroll_state = self
                .scroll_state
                .position(self.scroll_position)
                .content_length(self.scroll_max);

            self.code_block_hitboxes = code_block_hitboxes;
            self.message_hitboxes = message_hitboxes;

            paragraph.render(area, buf);
            StatefulWidget::render(
                Scrollbar::new(ratatui::widgets::ScrollbarOrientation::VerticalRight),
                area,
                buf,
                &mut self.scroll_state,
            );
        } else {
            // No messages, render the ascii art logo + /session message
            block.render(area, buf);
            let mut lines = r#" ____  _                     _   _     
| __ )(_)___ _ __ ___  _   _| |_| |__  
|  _ \| / __| '_ ` _ \| | | | __| '_ \ 
| |_) | \__ \ | | | | | |_| | |_| | | |
|____/|_|___/_| |_| |_|\__,_|\__|_| |_|
"#
            .split('\n')
            .map(|line| Line::styled(line, Style::default().fg(ratatui::style::Color::Magenta)))
            .collect::<Vec<_>>();
            lines.push(Line::raw("Use `/session` to change session"));
            let paragraph = Paragraph::new(lines);
            let area = centered_paragraph(&paragraph, area.inner(Margin::new(0, 1)));
            Clear.render(area, buf);
            paragraph.render(area, buf);
        }
    }
}

#[derive(Clone, Debug)]
struct DiffReviewWidget {
    diff: String,
    commit_message: Option<String>,
    msg_id: u64,
    v_scroll_position: usize,
    v_scroll_max: usize,
    v_scroll_state: ratatui::widgets::ScrollbarState,
    h_scroll_position: usize,
    h_scroll_max: usize,
    h_scroll_state: ratatui::widgets::ScrollbarState,
}

impl DiffReviewWidget {
    fn new(diff: String, msg_id: u64, commit_message: Option<String>) -> Self {
        Self {
            diff,
            commit_message,
            msg_id,
            v_scroll_position: 0,
            v_scroll_max: 0,
            v_scroll_state: ratatui::widgets::ScrollbarState::default(),
            h_scroll_position: 0,
            h_scroll_max: 0,
            h_scroll_state: ratatui::widgets::ScrollbarState::default(),
        }
    }
}

impl Widget for &mut DiffReviewWidget {
    fn render(self, area: ratatui::layout::Rect, buf: &mut ratatui::buffer::Buffer) {
        let lines = self
            .diff
            .lines()
            .map(|line| {
                let mut ui_line = Line::raw(line);
                if line.starts_with('+') && !line.starts_with("+++") {
                    ui_line = ui_line.green();
                } else if line.starts_with('-') && !line.starts_with("---") {
                    ui_line = ui_line.red();
                }
                ui_line
            })
            .collect::<Vec<_>>();
        let paragraph = Paragraph::new(lines)
            .block(Block::bordered().title(vec![
                "Review Diff ".into(),
                Span::styled("(y to commit, n to revert)", ratatui::style::Color::Yellow),
            ]))
            .scroll((self.v_scroll_position as u16, self.h_scroll_position as u16));

        let nlines = self.diff.lines().count();
        self.v_scroll_max = nlines.max(area.height as usize) - (area.height as usize);
        self.v_scroll_state = self
            .v_scroll_state
            .position(self.v_scroll_position)
            .content_length(self.v_scroll_max);

        self.h_scroll_max = self
            .diff
            .lines()
            .map(|l| l.len())
            .max()
            .unwrap_or(0)
            .max(area.width as usize)
            - (area.width as usize);
        self.h_scroll_state = self
            .h_scroll_state
            .position(self.h_scroll_position)
            .content_length(self.h_scroll_max);

        let area = centered_paragraph(&paragraph, area);
        Clear.render(area, buf);
        paragraph.render(area, buf);
        StatefulWidget::render(
            Scrollbar::new(ratatui::widgets::ScrollbarOrientation::VerticalRight),
            area,
            buf,
            &mut self.v_scroll_state,
        );
        StatefulWidget::render(
            Scrollbar::new(ratatui::widgets::ScrollbarOrientation::HorizontalBottom),
            area,
            buf,
            &mut self.h_scroll_state,
        );
    }
}

#[derive(Clone, Debug)]
struct SelectSessionWidget {
    sessions: Vec<api::ChatSession>,
    current_session: api::ChatSession,
    selected_idx: usize,
    v_scroll_position: usize,
}

impl Widget for &mut SelectSessionWidget {
    fn render(self, area: ratatui::layout::Rect, buf: &mut ratatui::buffer::Buffer) {
        let mut lines = vec![];
        for (idx, session) in self.sessions.iter().enumerate() {
            let mut line = Line::raw(session.name());
            if idx == self.selected_idx {
                line = line.style(Style::default().bg(ratatui::style::Color::Blue));
            }
            lines.push(line);
        }
        let paragraph = Paragraph::new(lines)
            .block(
                Block::bordered()
                    .title("Select Session")
                    .padding(Padding::new(1, 0, 0, 0)),
            )
            .scroll((self.v_scroll_position as u16, 0));

        if self.selected_idx >= (area.height - 2) as usize + self.v_scroll_position {
            self.v_scroll_position = self.selected_idx - (area.height - 2) as usize + 1;
        } else if self.selected_idx < self.v_scroll_position {
            self.v_scroll_position = self.selected_idx;
        }
        let mut v_scroll_state = ScrollbarState::default()
            .content_length(self.sessions.len() - (area.height as usize - 2))
            .position(self.v_scroll_position);

        let area = centered_paragraph(&paragraph, area);
        Clear.render(area, buf);
        paragraph.render(area, buf);
        StatefulWidget::render(
            Scrollbar::new(ratatui::widgets::ScrollbarOrientation::VerticalRight),
            area,
            buf,
            &mut v_scroll_state,
        );
    }
}

#[derive(Clone, Debug)]
enum AppState {
    Chat,
    SelectSession(SelectSessionWidget),
    Popup(String, String),
    ReviewDiff(DiffReviewWidget),
    // Sort of a hacky way to feed state from the event input loop back up
    ChangeSession(api::ChatSession),
    Exit,
}

struct App {
    repo_path: PathBuf,
    user: api::User,

    /// Chat is always present in the background so this is not kept in the state
    chat_history: ChatHistoryWidget,

    /// Current chatbox input
    input: tui_textarea::TextArea<'static>,

    client: APIClient,
    ws_stream: Option<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    >,
    project: api::Project,
    feature: api::Feature,
    session: api::ChatSession,
    state: Arc<Mutex<AppState>>,
}

impl App {
    fn new(
        repo_path: &Path,
        project: &api::Project,
        feature: &api::Feature,
        session: &api::ChatSession,
        current_user: &api::User,
        sessions: Vec<api::ChatSession>,
        chat_history: &[ChatMessage],
        ws_stream: tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        client: &APIClient,
    ) -> Self {
        let mut x = Self {
            repo_path: repo_path.to_path_buf(),
            user: current_user.clone(),
            chat_history: ChatHistoryWidget {
                messages: Arc::new(Mutex::new(chat_history.to_vec())),
                scroll_position: 0,
                scroll_max: 0,
                scroll_state: ratatui::widgets::ScrollbarState::default(),
                code_block_hitboxes: vec![],
                message_hitboxes: vec![],
                sessions,
                session: session.clone(),
                selection: None,
            },
            input: tui_textarea::TextArea::default(),
            client: client.clone(),
            ws_stream: Some(ws_stream),
            project: project.clone(),
            feature: feature.clone(),
            session: session.clone(),
            state: Arc::new(Mutex::new(AppState::Chat)),
        };
        x.clear_input();
        x
    }

    fn clear_input(&mut self) {
        self.input = tui_textarea::TextArea::default();
        self.input.set_block(Block::bordered().title("Message"));
        self.input
            .set_placeholder_text(" Use Alt/Option + Enter to add a newline");
        self.input.set_cursor_line_style(Style::default());
    }

    async fn read_loop(
        read: &mut SplitStream<WebSocketStream<MaybeTlsStream<TcpStream>>>,
        write: &mpsc::Sender<tokio_tungstenite::tungstenite::Message>,
        scrollback: Arc<Mutex<Vec<ChatMessage>>>,
        repo_path: &Path,
        state: Arc<Mutex<AppState>>,
    ) -> Result<()> {
        loop {
            let message = match read.try_next().await {
                Err(e) => {
                    return Err(e.into());
                }
                Ok(None) => {
                    return Ok(());
                }
                Ok(Some(message)) => message,
            };
            if let Message::Ping(_) = message {
                continue;
            }
            if let Message::Close(_) = message {
                return Ok(());
            }
            let data: api::ws::Message =
                serde_json::from_str(&message.into_text().unwrap()).unwrap();
            match data {
                api::ws::Message::Chat(api::ws::ChatMessage { message, .. }) => {
                    let stuff: api::ws::ChatMessageBody = serde_json::from_str(&message).unwrap();
                    match stuff {
                        api::ws::ChatMessageBody::StreamingToken { token, .. } => {
                            let mut scrollback = scrollback.lock().unwrap();
                            // Daneel snapshot resumption
                            if scrollback.len() == 0 {
                                scrollback.push(ChatMessage::new(ChatMessageUser::AI, ""));
                            }
                            let last_msg = scrollback.last_mut().unwrap();
                            loop {
                                match last_msg.blocks.last() {
                                    Some(MessageBlock::Thinking(_)) => {
                                        last_msg.blocks.pop();
                                    }
                                    _ => break,
                                }
                            }
                            last_msg.append(&token.text);
                        }
                        api::ws::ChatMessageBody::PartialMessage { partial_message } => {
                            let mut scrollback = scrollback.lock().unwrap();
                            let msg = ChatMessage::new(ChatMessageUser::AI, &partial_message);
                            // Basically just to support snapshot resumption in daneel
                            if scrollback.len() > 0 {
                                let last = scrollback.last_mut().unwrap();
                                *last = msg;
                            } else {
                                scrollback.push(msg);
                            }
                        }
                        api::ws::ChatMessageBody::FinalizedMessage {
                            generated_text,
                            output_modified_files,
                            commit_message,
                            id,
                            ..
                        } => {
                            {
                                let mut scrollback = scrollback.lock().unwrap();
                                let last = scrollback.last_mut().unwrap();
                                *last = ChatMessage::new(ChatMessageUser::AI, &generated_text);
                                last.finalized = true;
                            }

                            if let Some(diff) =
                                process_chat_message(&repo_path, &output_modified_files).unwrap()
                            {
                                if !diff.is_empty() {
                                    let mut state = state.lock().unwrap();
                                    *state = AppState::ReviewDiff(DiffReviewWidget::new(
                                        diff,
                                        id,
                                        commit_message,
                                    ));
                                }
                            }
                        }
                    }
                }
                api::ws::Message::ResponseState(resp) => {
                    let mut scrollback = scrollback.lock().unwrap();
                    let last = scrollback.last_mut().unwrap();
                    match last.blocks.last() {
                        // Only add a new thinking block if the text has actually changed
                        Some(MessageBlock::Thinking(last_state)) if *last_state == resp.state => {}
                        _ => {
                            last.blocks.push(MessageBlock::Thinking(resp.state.clone()));
                        }
                    }
                }
                api::ws::Message::RunCommand(cmd) => {
                    let mut scrollback = scrollback.lock().unwrap();
                    let last = scrollback.last_mut().unwrap();
                    last.blocks.push(MessageBlock::Thinking(format!(
                        "Running command: {}",
                        cmd.command
                    )));
                    let should_revert =
                        process_chat_message(&repo_path, &cmd.output_modified_files)?.is_some();
                    let repo_path = repo_path.to_path_buf();
                    let write_ = write.clone();
                    tokio::spawn(async move {
                        let proc = tokio::process::Command::new("sh")
                            .arg("-c")
                            .arg(&cmd.command)
                            .current_dir(&repo_path)
                            .output()
                            .await;
                        if should_revert {
                            let _ = revert(&repo_path);
                        }
                        let _ = write_
                            .send(Message::Text(
                                serde_json::to_string(&api::ws::Message::RunCommandResponse(
                                    RunCommandResponse {
                                        exit_code: proc
                                            .as_ref()
                                            .map_or(1, |p| p.status.code().unwrap_or(1)),
                                        stdout: proc.as_ref().map_or("".to_string(), |p| {
                                            String::from_utf8_lossy(&p.stdout).to_string()
                                        }),
                                        stderr: match proc {
                                            Ok(p) => String::from_utf8_lossy(&p.stderr).to_string(),
                                            Err(e) => e.to_string(),
                                        },
                                    },
                                ))
                                .unwrap(),
                            ))
                            .await;
                    });
                }
                api::ws::Message::Error(err) => {
                    return Err(anyhow!(err));
                }
                _ => {}
            }
        }
    }

    async fn run(&mut self) -> Result<Option<api::ChatSession>> {
        let mut terminal = terminal::init()?;

        let (mut write_sink, mut read) = self.ws_stream.take().unwrap().split();
        let (dead_tx, mut dead_rx) = tokio::sync::oneshot::channel();

        let (write, mut write_source) = mpsc::channel(1);
        tokio::spawn(async move {
            while let Some(msg) = write_source.recv().await {
                write_sink.send(msg).await.unwrap();
            }
            write_sink.close().await.unwrap();
        });

        let scrollback = self.chat_history.messages.clone();
        let repo_path = self.repo_path.clone();
        let state = self.state.clone();
        let write_ = write.clone();
        tokio::spawn(async move {
            let res =
                Self::read_loop(&mut read, &write_, scrollback.clone(), &repo_path, state).await;
            let _ = dead_tx.send(res);
        });

        let mut last_draw = Instant::now();
        let mut last_input = Instant::now();
        let mut input_delay = VecDeque::new();
        loop {
            let state = { self.state.lock().unwrap().clone() };
            if let AppState::Exit = state {
                return Ok(None);
            }
            if let AppState::ChangeSession(new_session) = state {
                return Ok(Some(new_session));
            }
            if let Ok(res) = dead_rx.try_recv() {
                return res.map(|_| None);
            }
            if last_draw.elapsed() > Duration::from_millis(40) {
                last_draw = Instant::now();
                terminal.draw(|frame| {
                    ui(
                        frame,
                        self.state.clone(),
                        &mut self.chat_history,
                        &self.input,
                    )
                })?;
            }
            if !tokio::task::spawn_blocking(move || event::poll(Duration::from_millis(40)))
                .await??
            {
                continue;
            }
            // TODO: bracketed paste mode
            if input_delay.len() == 3 {
                input_delay.pop_front();
            }
            input_delay.push_back(last_input.elapsed());
            last_input = Instant::now();
            match state {
                AppState::Exit => {
                    return Ok(None);
                }
                AppState::ChangeSession(new_session) => {
                    return Ok(Some(new_session));
                }
                AppState::ReviewDiff(diff) => match event::read()? {
                    Event::Key(key) if key.kind == event::KeyEventKind::Press => match key.code {
                        KeyCode::Char('y') => {
                            commit(&self.repo_path, diff.commit_message.as_deref())?;
                            let client = self.client.clone();
                            let project = self.project.id;
                            let feature = self.feature.id;
                            let message_id = diff.msg_id;
                            tokio::spawn(async move {
                                let _ = client
                                    .post(&format!(
                                        "/projects/{}/features/{}/chat/accepted",
                                        project, feature,
                                    ))
                                    .json(&api::GenerationAcceptedRequest {
                                        message_id,
                                        accepted: true,
                                    })
                                    .send()
                                    .await;
                            });
                            let mut state = self.state.lock().unwrap();
                            *state = AppState::Chat;
                        }
                        KeyCode::Char('n') | KeyCode::Esc => {
                            revert(&self.repo_path)?;
                            let client = self.client.clone();
                            let project = self.project.id;
                            let feature = self.feature.id;
                            let message_id = diff.msg_id;
                            tokio::spawn(async move {
                                let _ = client
                                    .post(&format!(
                                        "/projects/{}/features/{}/chat/accepted",
                                        project, feature,
                                    ))
                                    .json(&api::GenerationAcceptedRequest {
                                        message_id,
                                        accepted: false,
                                    })
                                    .send()
                                    .await;
                            });
                            let mut state = self.state.lock().unwrap();
                            *state = AppState::Chat;
                        }
                        KeyCode::Char(' ') => {
                            let mut state = self.state.lock().unwrap();
                            if let AppState::ReviewDiff(diff_widget) = &mut *state {
                                diff_widget.v_scroll_position = diff_widget
                                    .v_scroll_position
                                    .saturating_add(10)
                                    .clamp(0, diff_widget.v_scroll_max);
                            }
                        }
                        KeyCode::Down => {
                            let mut state = self.state.lock().unwrap();
                            if let AppState::ReviewDiff(diff_widget) = &mut *state {
                                diff_widget.v_scroll_position = diff_widget
                                    .v_scroll_position
                                    .saturating_add(1)
                                    .clamp(0, diff_widget.v_scroll_max);
                            }
                        }
                        KeyCode::Up => {
                            let mut state = self.state.lock().unwrap();
                            if let AppState::ReviewDiff(diff_widget) = &mut *state {
                                diff_widget.v_scroll_position = diff_widget
                                    .v_scroll_position
                                    .saturating_sub(1)
                                    .clamp(0, diff_widget.v_scroll_max);
                            }
                        }
                        KeyCode::Left => {
                            let mut state = self.state.lock().unwrap();
                            if let AppState::ReviewDiff(diff_widget) = &mut *state {
                                diff_widget.h_scroll_position = diff_widget
                                    .h_scroll_position
                                    .saturating_sub(1)
                                    .clamp(0, diff_widget.h_scroll_max);
                            }
                        }
                        KeyCode::Right => {
                            let mut state = self.state.lock().unwrap();
                            if let AppState::ReviewDiff(diff_widget) = &mut *state {
                                diff_widget.h_scroll_position = diff_widget
                                    .h_scroll_position
                                    .saturating_add(1)
                                    .clamp(0, diff_widget.h_scroll_max);
                            }
                        }
                        _ => {}
                    },
                    Event::Mouse(mouse) => match mouse.kind {
                        event::MouseEventKind::ScrollUp => {
                            // TODO: if cursor row within message field, self.input.scroll instead
                            let mut state = self.state.lock().unwrap();
                            if let AppState::ReviewDiff(diff_widget) = &mut *state {
                                diff_widget.v_scroll_position = diff_widget
                                    .v_scroll_position
                                    .saturating_sub(1)
                                    .clamp(0, diff_widget.v_scroll_max);
                            }
                        }
                        event::MouseEventKind::ScrollDown => {
                            let mut state = self.state.lock().unwrap();
                            if let AppState::ReviewDiff(diff_widget) = &mut *state {
                                diff_widget.v_scroll_position = diff_widget
                                    .v_scroll_position
                                    .saturating_add(1)
                                    .clamp(0, diff_widget.v_scroll_max);
                            }
                        }
                        _ => {}
                    },
                    _ => {}
                },
                AppState::Popup(_, _) => {
                    if let Event::Key(_) = event::read()? {
                        let mut state = self.state.lock().unwrap();
                        *state = AppState::Chat;
                    }
                }
                AppState::SelectSession(widget) => match event::read()? {
                    Event::Key(key) if key.kind == event::KeyEventKind::Press => match key.code {
                        KeyCode::Up => {
                            let mut state = self.state.lock().unwrap();
                            if let AppState::SelectSession(widget) = &mut *state {
                                widget.selected_idx = widget.selected_idx.saturating_sub(1);
                            }
                        }
                        KeyCode::Down => {
                            if let AppState::SelectSession(widget) =
                                &mut *self.state.lock().unwrap()
                            {
                                widget.selected_idx = widget
                                    .selected_idx
                                    .saturating_add(1)
                                    .clamp(0, widget.sessions.len() - 1);
                            }
                        }
                        KeyCode::Esc => {
                            let mut state = self.state.lock().unwrap();
                            *state = AppState::Chat;
                        }
                        KeyCode::Enter => {
                            let widget = widget;
                            let session = widget.sessions[widget.selected_idx].clone();
                            let mut state = self.state.lock().unwrap();
                            *state = AppState::ChangeSession(session);
                        }
                        _ => {}
                    },
                    _ => {}
                },
                AppState::Chat => match event::read()? {
                    Event::Mouse(mouse) => match mouse.kind {
                        event::MouseEventKind::ScrollUp => {
                            self.chat_history.scroll_position =
                                self.chat_history.scroll_position.saturating_sub(1);
                        }
                        event::MouseEventKind::ScrollDown => {
                            self.chat_history.scroll_position = self
                                .chat_history
                                .scroll_position
                                .saturating_add(1)
                                .clamp(0, self.chat_history.scroll_max);
                        }
                        /*
                        event::MouseEventKind::Down(btn) if btn == MouseButton::Left => {
                            let col = mouse.column as usize - 2; // border + padding
                            let row = mouse.row as usize - 2 + self.chat_history.scroll_position;
                            self.chat_history.selection = Some(((col, row), (col, row)));
                        }
                        event::MouseEventKind::Drag(btn) if btn == MouseButton::Left => {
                            let col = mouse.column as usize - 2;
                            let row = mouse.row as usize - 2 + self.chat_history.scroll_position;
                            if let Some((start, _)) = self.chat_history.selection {
                                self.chat_history.selection = Some((start, (col, row)));
                            } else {
                                // dont think this should happen
                                self.chat_history.selection = Some(((col, row), (col, row)));
                            }
                        }
                        */
                        event::MouseEventKind::Up(btn) if btn == MouseButton::Left => {
                            // Only expand when not dragging/making a selection
                            /*
                            if let Some((start, end)) = self.chat_history.selection {
                                if start == end {
                                    self.chat_history.selection = None;
                            */
                            let mut messages = self.chat_history.messages.lock().unwrap();

                            if let Ok(mut clipboard_ctx) = copypasta::ClipboardContext::new() {
                                for ((start, end), block) in self
                                    .chat_history
                                    .message_hitboxes
                                    .iter()
                                    .zip(messages.iter())
                                {
                                    // -1 for the border of chat history
                                    if (*start as isize
                                        - self.chat_history.scroll_position as isize)
                                        == (mouse.row as isize) - 1
                                        && (mouse.column as usize == 1
                                            || mouse.column as usize == 2)
                                    {
                                        clipboard_ctx.set_contents(block.raw.clone()).unwrap();
                                    }
                                }
                            }

                            let mut hitboxes_iter = self.chat_history.code_block_hitboxes.iter();
                            for msg in messages.iter_mut() {
                                for block in &mut msg.blocks {
                                    match block {
                                        MessageBlock::Code(code) => {
                                            let (start, end) = hitboxes_iter.next().unwrap();
                                            // -1 for the border of chat history
                                            if (*start as isize
                                                - self.chat_history.scroll_position as isize)
                                                <= (mouse.row as isize) - 1
                                                && (*end as isize
                                                    - self.chat_history.scroll_position as isize)
                                                    > (mouse.row as isize) - 1
                                            {
                                                code.folded = !code.folded;
                                                msg.block_line_cache.1.clear();
                                            }
                                        }
                                        _ => {}
                                    }
                                }
                            }
                            /*
                                                           }
                                                       }
                            */
                        }
                        _ => {}
                    },
                    Event::Key(key) if key.kind == event::KeyEventKind::Press => match key.code {
                        /*
                        KeyCode::Char('c')
                            if key.modifiers.contains(event::KeyModifiers::CONTROL) =>
                        {
                        } */
                        KeyCode::Char('d')
                            if key.modifiers.contains(event::KeyModifiers::CONTROL) =>
                        {
                            let mut state = self.state.lock().unwrap();
                            *state = AppState::Exit;
                        }
                        KeyCode::Esc => {
                            let mut state = self.state.lock().unwrap();
                            *state = AppState::Exit;
                        }
                        KeyCode::Enter => {
                            // ALT+enter for manual newlines
                            // or if this is a paste (in which case input delay is very short)
                            if key.modifiers.contains(event::KeyModifiers::ALT)
                                || input_delay.iter().all(|d| d < &Duration::from_millis(1))
                            {
                                self.input.input(key);
                            } else {
                                let last_generation_done = self
                                    .chat_history
                                    .messages
                                    .lock()
                                    .unwrap()
                                    .last()
                                    .map_or(true, |msg| msg.finalized);
                                if last_generation_done {
                                    self.handle_chat_input(&write).await?;
                                }
                            }
                        }
                        _ => {
                            self.input.input(key);
                        }
                    },
                    _ => (),
                },
            }
        }
    }

    async fn handle_chat_input(
        &mut self,
        write: &mpsc::Sender<tokio_tungstenite::tungstenite::Message>,
    ) -> Result<()> {
        if self.input.is_empty() {
            return Ok(());
        }
        let input = self.input.lines().to_vec().join("\n");
        if input.starts_with('/') {
            {
                let mut state = self.state.lock().unwrap();
                match input.split(' ').next().unwrap() {
                    "/exit" | "/quit" => {
                        *state = AppState::Exit;
                    }
                    "/help" => {
                        *state = AppState::Popup(
                            "Help".to_string(),
                            r#"/exit, /quit, or Esc: Exit the chat
/docs: Open the Bismuth documentation
/new-session [NAME]: Start a new session
/session [NAME]: Switch to a different session
/rename-session <NAME>: Rename the current session
/feedback <DESCRIPTION>: Send us feedback
/help: Show this help"#
                                .to_string(),
                        );
                    }
                    "/docs" => {
                        open::that_detached("https://app.bismuth.cloud/docs")?;
                    }
                    "/new-session" => {
                        let session_name = input.split_once(' ').map(|(_, msg)| msg);
                        let session = self
                            .client
                            .post(&format!(
                                "/projects/{}/features/{}/chat/sessions",
                                self.project.id, self.feature.id
                            ))
                            .json(&json!({ "name": session_name }))
                            .send()
                            .await?
                            .error_body_for_status()
                            .await?
                            .json()
                            .await?;
                        *state = AppState::ChangeSession(session);
                    }
                    "/rename-session" => {
                        let name = input.split_once(' ').map(|(_, msg)| msg);
                        match name {
                            None => {
                                *state = AppState::Popup(
                                    "Error".to_string(),
                                    "\n\n    You must provide a new name    \n\n".to_string(),
                                );
                            }
                            Some(name) => {
                                self.client
                                    .put(&format!(
                                        "/projects/{}/features/{}/chat/sessions/{}",
                                        self.project.id, self.feature.id, self.session.id
                                    ))
                                    .json(&json!({ "name": name }))
                                    .send()
                                    .await?
                                    .error_body_for_status()
                                    .await?;
                            }
                        }
                    }
                    "/change-session" | "/switch-session" | "/session" => {
                        let name = input.split_once(' ').map(|(_, msg)| msg);
                        match name {
                            None => {
                                *state = AppState::SelectSession(SelectSessionWidget {
                                    sessions: self.chat_history.sessions.clone(),
                                    current_session: self.session.clone(),
                                    selected_idx: 0,
                                    v_scroll_position: 0,
                                })
                            }
                            Some(name) => {
                                match self.chat_history.sessions.iter().find(|s| s.name() == name) {
                                    None => {
                                        *state = AppState::Popup(
                                            "Error".to_string(),
                                            "\n\n    There's no session with that name    \n\n"
                                                .to_string(),
                                        );
                                    }
                                    Some(session) => {
                                        *state = AppState::ChangeSession(session.clone());
                                    }
                                }
                            }
                        }
                    }
                    "/feedback" => {
                        let msg = input.split_once(' ').map(|(_, msg)| msg);
                        match msg {
                            Some(msg) => {
                                self.client
                                    .post(&format!(
                                        "/projects/{}/features/{}/bugreport",
                                        self.project.id, self.feature.id
                                    ))
                                    .json(&json!({ "message": msg }))
                                    .send()
                                    .await?;
                                *state = AppState::Popup(
                                    "Confirmation".to_string(),
                                    "\n\n    Feedback submitted. Thank you!    \n\n".to_string(),
                                );
                            }
                            None => {
                                *state = AppState::Popup(
                                "Error".to_string(),
                                "\n\n    You must provide a message in the /feedback command    \n\n".to_string(),
                            );
                            }
                        }
                    }
                    // eh idk if we want this, seems like a good way to lose things even with the name check
                    "/undo" => {
                        let repo = git2::Repository::open(&self.repo_path)?;
                        let last = repo.revparse_single("HEAD~1")?;
                        if last.peel_to_commit()?.author().name().unwrap() == "Bismuth" {
                            repo.reset(
                                &repo.revparse_single("HEAD~1")?,
                                git2::ResetType::Hard,
                                Some(git2::build::CheckoutBuilder::new().force()),
                            )?;
                        }
                    }
                    _ => {
                        *state = AppState::Popup(
                            "Error".to_string(),
                            "\n\n    Unrecognized command    \n\n".to_string(),
                        );
                    }
                }
            }
            self.clear_input();
            return Ok(());
        }

        {
            let mut scrollback = self.chat_history.messages.lock().unwrap();

            let mut msg = ChatMessage::new(ChatMessageUser::User(self.user.name.clone()), &input);
            msg.finalized = true;
            scrollback.push(msg);

            let mut ai_msg = ChatMessage::new(ChatMessageUser::AI, "");
            ai_msg.blocks.clear();
            ai_msg
                .blocks
                .push(MessageBlock::Thinking("Planning".to_string()));
            scrollback.push(ai_msg);

            let modified_files = list_changed_files(&self.repo_path)?
                .into_iter()
                .map(|path| {
                    let content = std::fs::read_to_string(&self.repo_path.join(&path))
                        .unwrap_or("".to_string());
                    api::ws::ChatModifiedFile {
                        name: path.file_name().unwrap().to_str().unwrap().to_string(),
                        project_path: path.to_str().unwrap().to_string(),
                        content,
                    }
                })
                .collect();

            write
                .send(Message::Text(
                    serde_json::to_string(&api::ws::Message::Chat(api::ws::ChatMessage {
                        message: input.clone(),
                        modified_files,
                        request_type_analysis: false,
                    }))?
                    .into(),
                ))
                .await?;
        }

        self.clear_input();

        Ok(())
    }
}

pub async fn start_chat(
    current_user: &api::User,
    project: &api::Project,
    feature: &api::Feature,
    sessions: Vec<api::ChatSession>,
    session: &api::ChatSession,
    repo_path: &Path,
    client: &APIClient,
) -> Result<()> {
    let repo_path = repo_path.to_path_buf();

    let mut session = session.clone();

    let status = loop {
        let scrollback: Vec<ChatMessage> = client
            .get(&format!(
                "/projects/{}/features/{}/chat/sessions/{}/list",
                project.id, feature.id, session.id
            ))
            .send()
            .await?
            .error_body_for_status()
            .await?
            .json::<Vec<api::ChatMessage>>()
            .await?
            .into_iter()
            .map(Into::into)
            .collect();

        let url = websocket_url(&client.base_url);
        let (mut ws_stream, _) = connect_async(url).await.expect("Failed to connect");

        ws_stream
            .send(Message::Text(
                serde_json::to_string(&api::ws::Message::Auth(api::ws::AuthMessage {
                    feature_id: feature.id.clone(),
                    session_id: session.id.clone(),
                    token: client.token.clone(),
                }))?
                .into(),
            ))
            .await?;

        debug!("Connected to chat");

        let mut app = App::new(
            &repo_path,
            project,
            feature,
            &session,
            current_user,
            sessions.clone(),
            &scrollback,
            ws_stream,
            client,
        );

        let status = app.run().await;
        match status {
            Ok(Some(new_session)) => {
                session = new_session;
                continue;
            }
            Ok(None) => {
                break Ok(());
            }
            Err(e) => {
                break Err(e);
            }
        }
    };

    terminal::restore();

    status
}

fn ui(
    frame: &mut ratatui::Frame,
    state: Arc<Mutex<AppState>>,
    chat_history: &mut ChatHistoryWidget,
    input: &tui_textarea::TextArea,
) {
    let _ = match &*state.lock().unwrap() {
        AppState::Chat => {
            ratatui::crossterm::execute!(std::io::stdout(), SetCursorStyle::BlinkingBlock)
        }
        _ => {
            ratatui::crossterm::execute!(std::io::stdout(), SetCursorStyle::SteadyBlock)
        }
    };

    let vertical = ratatui::layout::Layout::vertical([
        ratatui::layout::Constraint::Percentage(100),
        ratatui::layout::Constraint::Min((input.lines().len().clamp(1, 3) + 2) as u16),
    ]);
    let [history_area, input_area] = vertical.areas(frame.area());

    frame.render_widget(chat_history, history_area);

    frame.render_widget(input, input_area);

    let mut state = state.lock().unwrap();
    match &mut *state {
        AppState::ReviewDiff(diff_widget) => {
            frame.render_widget(diff_widget, frame.area());
        }
        AppState::Popup(title, text) => {
            let paragraph = Paragraph::new(text.clone()).block(Block::bordered().title(vec![
                format!("{} ", title).into(),
                Span::styled("(press any key to close)", ratatui::style::Color::Yellow),
            ]));
            let area = centered_paragraph(&paragraph, frame.area());
            frame.render_widget(Clear, area);
            frame.render_widget(paragraph, area);
        }
        AppState::SelectSession(widget) => {
            frame.render_widget(widget, frame.area());
        }
        _ => {}
    }
}

fn centered_paragraph(paragraph: &Paragraph, r: Rect) -> Rect {
    // +2 for border
    let width = (paragraph.line_width() + 2).min(r.width as usize) as u16;
    let height = (paragraph.line_count(width) + 2).min(r.height as usize) as u16;

    let popup_layout = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(height),
        Constraint::Fill(1),
    ])
    .split(r);

    Layout::horizontal([
        Constraint::Fill(1),
        Constraint::Length(width),
        Constraint::Fill(1),
    ])
    .split(popup_layout[1])[1]
}

mod terminal {
    use std::{io, process::Command};

    use ratatui::{
        backend::CrosstermBackend,
        crossterm::{
            event::{
                DisableMouseCapture, EnableMouseCapture, KeyboardEnhancementFlags,
                PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
            },
            execute,
            terminal::{
                disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
            },
        },
    };

    /// A type alias for the terminal type used in this example.
    pub type Terminal = ratatui::Terminal<CrosstermBackend<io::Stdout>>;

    pub fn init() -> io::Result<Terminal> {
        set_panic_hook();
        enable_raw_mode()?;
        execute!(
            io::stdout(),
            EnterAlternateScreen,
            EnableMouseCapture,
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES),
        )?;
        let backend = CrosstermBackend::new(io::stdout());
        Terminal::new(backend)
    }

    fn set_panic_hook() {
        let hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore();
            hook(info);
        }));
    }

    /// Restores the terminal to its original state.
    pub fn restore() {
        if let Err(err) = disable_raw_mode() {
            eprintln!("error disabling raw mode: {err}");
        }
        if let Err(err) = execute!(
            io::stdout(),
            LeaveAlternateScreen,
            DisableMouseCapture,
            PopKeyboardEnhancementFlags,
        ) {
            eprintln!("error leaving alternate screen: {err}");
        }
        // Reset cursor shape
        let _ = Command::new("tput").arg("cnorm").status();
    }
}

mod test {
    use super::*;
    use std::collections::HashSet;
    use std::fs;

    fn copy_dir_all(src: impl AsRef<Path>, dst: impl AsRef<Path>) -> Result<()> {
        fs::create_dir_all(&dst)?;
        for entry in fs::read_dir(src)? {
            let entry = entry?;
            let ty = entry.file_type()?;
            if ty.is_dir() {
                copy_dir_all(entry.path(), dst.as_ref().join(entry.file_name()))?;
            } else {
                fs::copy(entry.path(), dst.as_ref().join(entry.file_name()))?;
            }
        }
        Ok(())
    }

    #[test]
    fn test_changed_files() -> Result<()> {
        let tmpdir = tempfile::tempdir()?;
        let remote_tmpdir = tempfile::tempdir()?;

        let repo = git2::Repository::init(tmpdir.path())?;
        let mut bismuth_remote = repo.remote("bismuth", remote_tmpdir.path().to_str().unwrap())?;

        let signature = git2::Signature::now("Bismuth-Test", "test@app.bismuth.cloud")?;
        {
            let mut index = repo.index()?;
            let tree_id = index.write_tree()?;
            let tree = repo.find_tree(tree_id)?;
            repo.commit(
                Some("HEAD"),
                &signature,
                &signature,
                "Initial commit",
                &tree,
                &[],
            )?;
        }

        fs::write(tmpdir.path().join("pushed"), "pushed")?;
        {
            let mut index = repo.index()?;
            index.add_all(&["*"], git2::IndexAddOption::DEFAULT, None)?;
            index.write()?;
            let tree_id = index.write_tree()?;
            let tree = repo.find_tree(tree_id)?;
            let head = repo.head()?;
            let parent_commit = repo.find_commit(head.target().unwrap())?;
            repo.commit(
                Some("HEAD"),
                &signature,
                &signature,
                "Test Commit",
                &tree,
                &[&parent_commit],
            )?;
        }
        copy_dir_all(&tmpdir, &remote_tmpdir)?;
        bismuth_remote.fetch(&["main"], None, None)?;

        fs::write(tmpdir.path().join("committed"), "committed")?;
        {
            let mut index = repo.index()?;
            index.add_all(&["*"], git2::IndexAddOption::DEFAULT, None)?;
            index.write()?;
            let tree_id = index.write_tree()?;
            let tree = repo.find_tree(tree_id)?;
            let head = repo.head()?;
            let parent_commit = repo.find_commit(head.target().unwrap())?;
            repo.commit(
                Some("HEAD"),
                &signature,
                &signature,
                "Test Commit",
                &tree,
                &[&parent_commit],
            )?;
        }

        fs::write(tmpdir.path().join("staged"), "staged")?;
        {
            let mut index = repo.index()?;
            index.add_all(&["*"], git2::IndexAddOption::DEFAULT, None)?;
            index.write()?;
        }

        fs::write(tmpdir.path().join("untracked"), "untracked")?;

        let changed_files: HashSet<_> = list_changed_files(tmpdir.path())?
            .into_iter()
            .map(|p| p.file_name().unwrap().to_str().unwrap().to_string())
            .collect();
        assert_eq!(
            changed_files,
            ["committed", "staged", "untracked"]
                .iter()
                .map(|f| f.to_string())
                .collect()
        );

        Ok(())
    }
}
