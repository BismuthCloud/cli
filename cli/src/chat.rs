use std::{
    cell::OnceCell,
    collections::HashSet,
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex, RwLock},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{anyhow, Result};
use copypasta::ClipboardProvider;
use derivative::Derivative;
use futures::{stream::SplitStream, SinkExt, StreamExt, TryStreamExt};
use log::{debug, trace};
use ratatui::{
    crossterm::{
        cursor::SetCursorStyle,
        event::{self, Event, KeyCode, MouseButton},
    },
    layout::{Constraint, Layout, Margin, Rect},
    style::Style,
    text::{Line, Span},
    widgets::{
        block::Title, Block, Borders, Clear, Padding, Paragraph, Scrollbar, ScrollbarState,
        StatefulWidget, Tabs, Widget,
    },
};
use serde_json::json;
use syntect::easy::HighlightLines;
use syntect::util::LinesWithEndings;
use tokio::{io::AsyncBufReadExt as _, net::TcpStream, sync::mpsc};
use tokio_stream::wrappers::LinesStream;
use tokio_tungstenite::{
    connect_async, tungstenite::protocol::Message, MaybeTlsStream, WebSocketStream,
};

use crate::{
    api::{
        self,
        ws::{ChatModifiedFile, RunCommandResponse},
    },
    tree::{FileTreeWidget, TreeNodeStyle},
    APIClient, ResponseErrorExt as _,
};
use crate::{bismuth_toml, tree::SimpleTreeNode};

/// List all files in the repository, excluding those blocked by the config.
fn list_all_files(repo_path: &Path) -> Result<Vec<String>> {
    let config = bismuth_toml::parse_config(repo_path)?;
    let globset = {
        let mut builder = globset::GlobSetBuilder::new();
        for glob in &config.chat.block_globs {
            builder.add(glob.clone());
        }
        builder.build().unwrap()
    };
    let mut files: HashSet<String> = Command::new("git")
        .arg("-C")
        .arg(&repo_path)
        .arg("ls-tree")
        .arg("-r")
        .arg("HEAD")
        .arg("--name-only")
        .output()
        .map_err(|e| anyhow!("Failed to run git ls-tree: {}", e))
        .and_then(|o| {
            if o.status.success() {
                Ok(o.stdout)
            } else {
                Err(anyhow!("git ls-tree failed (code={})", o.status))
            }
        })
        .and_then(|s| String::from_utf8(s).map_err(|e| anyhow!(e)))?
        .lines()
        .map(String::from)
        .collect();

    let repo = git2::Repository::open(repo_path)?;
    let statuses = repo.statuses(None)?;
    for status in statuses.iter() {
        match status.status() {
            git2::Status::WT_NEW
            | git2::Status::WT_MODIFIED
            | git2::Status::WT_DELETED
            | git2::Status::INDEX_NEW
            | git2::Status::INDEX_MODIFIED
            | git2::Status::INDEX_DELETED => {
                files.insert(status.path().unwrap().to_string());
            }
            git2::Status::WT_RENAMED | git2::Status::INDEX_RENAMED => {
                if let Some(stuff) = status.head_to_index() {
                    files.insert(
                        stuff
                            .old_file()
                            .path()
                            .unwrap()
                            .to_str()
                            .unwrap()
                            .to_string(),
                    );
                    files.insert(
                        stuff
                            .new_file()
                            .path()
                            .unwrap()
                            .to_str()
                            .unwrap()
                            .to_string(),
                    );
                }
                if let Some(stuff) = status.index_to_workdir() {
                    files.insert(
                        stuff
                            .old_file()
                            .path()
                            .unwrap()
                            .to_str()
                            .unwrap()
                            .to_string(),
                    );
                    files.insert(
                        stuff
                            .new_file()
                            .path()
                            .unwrap()
                            .to_str()
                            .unwrap()
                            .to_string(),
                    );
                }
            }
            _ => {}
        }
    }

    files = files
        .into_iter()
        .filter(|p| repo_path.join(p).is_file())
        .filter(|p| !globset.is_match(p))
        .collect();
    files.extend(
        config
            .chat
            .additional_files
            .iter()
            .filter(|p| repo_path.join(p).is_file())
            .map(String::from),
    );
    Ok(files.into_iter().collect())
}

/// List files that have changed in the working directory compared to the upstream branch.
fn list_changed_files(repo_path: &Path) -> Result<Vec<PathBuf>> {
    let repo = git2::Repository::open(repo_path)?;
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

    let config = bismuth_toml::parse_config(repo_path)?;
    changed_files.extend(
        config
            .chat
            .additional_files
            .iter()
            .filter(|p| repo_path.join(p).is_file())
            .map(PathBuf::from),
    );

    Ok(changed_files.into_iter().collect())
}

/// Return ChatModifiedFile objects for each file in the working directory that is untracked or staged.
/// This is used to capture changes that arise from command running and feed those back to the backend.
fn command_modified_files(repo_path: &Path) -> Result<Vec<ChatModifiedFile>> {
    let config = bismuth_toml::parse_config(repo_path)?;
    let block_globset = {
        let mut builder = globset::GlobSetBuilder::new();
        for glob in &config.chat.block_globs {
            builder.add(glob.clone());
        }
        builder.build()?
    };

    let repo = git2::Repository::open(repo_path)?;
    let statuses = repo.statuses(None)?;
    Ok(statuses
        .iter()
        .flat_map(|status| match status.status() {
            git2::Status::WT_NEW
            | git2::Status::WT_MODIFIED
            | git2::Status::INDEX_NEW
            | git2::Status::INDEX_MODIFIED => {
                vec![PathBuf::from(status.path().unwrap())]
            }
            git2::Status::WT_DELETED | git2::Status::INDEX_DELETED => {
                vec![PathBuf::from(status.path().unwrap())]
            }
            git2::Status::WT_RENAMED | git2::Status::INDEX_RENAMED => {
                if let Some(stuff) = status.head_to_index() {
                    vec![
                        PathBuf::from(stuff.old_file().path().unwrap()),
                        PathBuf::from(stuff.new_file().path().unwrap()),
                    ]
                } else if let Some(stuff) = status.index_to_workdir() {
                    vec![
                        PathBuf::from(stuff.old_file().path().unwrap()),
                        PathBuf::from(stuff.new_file().path().unwrap()),
                    ]
                } else {
                    vec![]
                }
            }
            _ => vec![],
        })
        .filter(|path| !block_globset.is_match(path.to_string_lossy().as_ref()))
        .chain(
            config
                .chat
                .additional_files
                .iter()
                .filter(|p| repo_path.join(p).is_file())
                .map(PathBuf::from),
        )
        .map(|path| ChatModifiedFile {
            name: path.file_name().unwrap().to_string_lossy().to_string(),
            project_path: path.to_string_lossy().to_string(),
            content: std::fs::read_to_string(repo_path.join(&path)).unwrap_or("".to_string()),
            deleted: Some(!repo_path.join(&path).exists()),
        })
        .collect())
}

const BISMUTH_AUTHOR: &str = "bismuthdev[bot]";

fn process_chat_message(
    repo_path: &Path,
    modified_files: &[ChatModifiedFile],
) -> Result<Option<String>> {
    if modified_files.is_empty() {
        return Ok(None);
    }

    let repo_path = std::fs::canonicalize(repo_path)?;
    let repo = git2::Repository::open(&repo_path)?;

    let mut index = repo.index()?;

    let head = repo.head()?;
    let parent_commit = repo.find_commit(head.target().unwrap())?;

    // Don't stack temp commits
    if parent_commit.message().unwrap_or("") != "Bismuth Temp Commit" {
        index.add_all(["*"], git2::IndexAddOption::DEFAULT, None)?;
        index.write()?;
        let tree_id = index.write_tree()?;
        let tree = repo.find_tree(tree_id)?;

        let signature =
            git2::Signature::now(BISMUTH_AUTHOR, "bismuthdev[bot]@users.noreply.github.com")?;
        repo.commit(
            Some("HEAD"),
            &signature,
            &signature,
            "Bismuth Temp Commit",
            &tree,
            &[&parent_commit],
        )?;
    }

    for mf in modified_files {
        if mf.deleted.unwrap_or_default() {
            trace!("Deleting file: {}", mf.project_path);
            let mut file_name = mf.project_path.as_str();
            file_name = file_name.trim_start_matches('/');
            let full_path = repo_path.join(file_name);
            if !full_path.starts_with(&repo_path) {
                return Err(anyhow!("Invalid file path"));
            }
            let _ = std::fs::remove_file(full_path);
        } else {
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
    }

    index.add_all(["*"], git2::IndexAddOption::DEFAULT, None)?;
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
        .and_then(|s| String::from_utf8(s).map_err(|e| anyhow!(e)))?;

    Ok(Some(diff))
}

fn commit(repo_path: &Path, message: Option<&str>) -> Result<()> {
    Command::new("git")
        .arg("-C")
        .arg(repo_path)
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

    let repo = git2::Repository::open(repo_path)?;
    let mut index = repo.index()?;
    index.add_all(["*"], git2::IndexAddOption::DEFAULT, None)?;
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
    let repo = git2::Repository::open(repo_path)?;

    let head = repo.head()?;
    let parent_commit = repo.find_commit(head.target().unwrap())?;

    // Don't revert unless this is a temp commit
    if parent_commit.message().unwrap_or("") != "Bismuth Temp Commit" {
        return Ok(());
    }

    let mut index = repo.index()?;
    index.remove_all(["*"], None)?;
    index.write()?;

    Command::new("git")
        .arg("-C")
        .arg(repo_path)
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
        .arg(repo_path)
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
        .arg(repo_path)
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
                .map(|(s, style)| (s.replace("\t", "    "), style))
                .collect(),
        }
    }
}

impl From<&str> for OwnedLine {
    fn from(s: &str) -> Self {
        Self {
            spans: vec![(s.replace("\t", "    "), Style::default())],
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

    diff_highlight_lines: Option<Vec<usize>>,
}

impl CodeBlock {
    fn new(filename: Option<&str>, language: Option<&str>, raw_code: &str) -> Self {
        Self {
            filename: filename.map(|f| f.to_string()),
            language: if let Some(language) = language {
                language.to_string()
            } else if let Some(filename) = filename {
                let ps = two_face::syntax::extra_newlines();
                ps.syntaxes()
                    .iter()
                    .find(|s| s.file_extensions.iter().any(|e| filename.ends_with(e)))
                    .map(|s| s.name.clone())
                    .unwrap_or("".to_string())
            } else {
                "".to_string()
            },
            raw_code: raw_code.to_string().replace("\t", "    "),
            lines: OnceCell::new(),
            folded: true,
            diff_highlight_lines: None,
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
            if let Some(diff_highlight_lines) = &self.diff_highlight_lines {
                LinesWithEndings::from(&self.raw_code)
                    .enumerate()
                    .map(|(line_no, line)| {
                        OwnedLine::from(
                            // Necessary so empty lines don't get double rendered
                            (if line != "\n" {
                                vec![if diff_highlight_lines.contains(&line_no) {
                                    ("█", Style::default().fg(ratatui::style::Color::Green))
                                } else {
                                    (" ", Style::default())
                                }]
                            } else {
                                vec![]
                            })
                            .into_iter()
                            .chain(h.highlight_line(line, &ps).unwrap().into_iter().map(
                                |(syntect_style, content)| {
                                    (
                                        content,
                                        Style {
                                            fg: {
                                                let syntect::highlighting::Color { r, g, b, a: _ } =
                                                    syntect_style.foreground;
                                                Some(ratatui::style::Color::Rgb(r, g, b))
                                            },
                                            bg: None,
                                            underline_color: None,
                                            add_modifier: ratatui::style::Modifier::empty(),
                                            sub_modifier: ratatui::style::Modifier::empty(),
                                        },
                                    )
                                },
                            ))
                            .collect::<Vec<_>>(),
                        )
                    })
                    .collect()
            } else {
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
                                            fg: {
                                                let syntect::highlighting::Color { r, g, b, a: _ } =
                                                    syntect_style.foreground;
                                                Some(ratatui::style::Color::Rgb(r, g, b))
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
            }
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
        Self::Text(text.split('\n').map(OwnedLine::from).collect())
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
            .replace("\n<BCODE>", "\n")
            .replace("\n</BCODE>", "\n")
            .replace("\r", "\n");
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
        spans.push((": ", ratatui::style::Style::default()));
        OwnedLine::from(spans)
    }

    fn parse_md(text: &str) -> Vec<MessageBlock> {
        let root = markdown::to_mdast(text, &markdown::ParseOptions::default()).unwrap();
        let mut blocks = match root.children() {
            Some(nodes) => nodes
                .iter()
                .filter_map(|block| match block {
                    markdown::mdast::Node::Code(code_block) => {
                        if !code_block.value.is_empty() {
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
        };
        if let Some(MessageBlock::Code(code_block)) = blocks.last_mut() {
            code_block.folded = false;
        }
        trace!("Parsed blocks: {:#?}", blocks);
        blocks
    }

    pub fn append(&mut self, token: &str) {
        self.raw += token;
        self.raw = self
            .raw
            .replace("\n<BCODE>", "\n")
            .replace("\n</BCODE>", "\n")
            .replace("\r", "\n");
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
    session: Arc<RwLock<api::ChatSession>>,
    feature: api::Feature,
    project: api::Project,
}

impl Widget for &mut ChatHistoryWidget {
    fn render(self, area: ratatui::layout::Rect, buf: &mut ratatui::buffer::Buffer) {
        let block = Block::new()
            .title(
                Title::from(format!(
                    " Chat History ({}/{}{}) ",
                    self.project.name,
                    self.feature.name,
                    match &self.session.read().unwrap()._name {
                        Some(name) => format!(" - {}", name),
                        None => "".to_string(),
                    },
                ))
                .alignment(ratatui::layout::Alignment::Left),
            )
            .title(
                Title::from(format!(
                    " Mode: {} ",
                    match &self.session.read().unwrap()._context_storage {
                        Some(storage) => {
                            if storage.mode == "single" {
                                "Lite".to_string()
                            } else if storage.mode == "chat" {
                                "Chat".to_string()
                            } else {
                                "Lite".to_string()
                            }
                        }
                        _ => "Lite".to_string(),
                    }
                ))
                .alignment(ratatui::layout::Alignment::Right),
            )
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
                                                    format!(
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
            self.scroll_max = nlines.saturating_sub(area.height as usize);
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
            let center_area = centered_paragraph(&paragraph, area.inner(Margin::new(0, 1)));
            Clear.render(center_area, buf);
            paragraph.render(center_area, buf);

            let legend_text = vec![
                "Ctrl+N: New session",
                "Ctrl+C: Exit",
                "Ctrl+M or /mode: Change mode",
                "/session: Switch session",
                "/feedback: Send feedback",
                "/help: Show full help",
            ];
            let legend_height = legend_text.len() as u16;
            let legend_width = legend_text.iter().map(|s| s.len()).max().unwrap() as u16;
            let legend_layout =
                Layout::vertical([Constraint::Length(legend_height), Constraint::Fill(1)])
                    .split(area.inner(Margin::new(2, 1)));
            let legend_rect =
                Layout::horizontal([Constraint::Fill(1), Constraint::Length(legend_width)])
                    .split(legend_layout[0])[1];
            let legend = Paragraph::new(
                legend_text
                    .into_iter()
                    .map(|t| Line::styled(t, Style::default().fg(ratatui::style::Color::DarkGray)))
                    .collect::<Vec<_>>(),
            )
            .alignment(ratatui::layout::Alignment::Right);
            legend.render(legend_rect, buf);
        }
    }
}

#[derive(Clone, Debug)]
struct DiffReviewWidget {
    lines: Vec<OwnedLine>,
    commit_message: Option<String>,
    msg_id: u64,
    can_apply: bool,
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
            lines: diff
                .lines()
                .map(|line| {
                    OwnedLine::from(vec![(
                        line,
                        if line.starts_with('+') && !line.starts_with("+++") {
                            Style::default().fg(ratatui::style::Color::Green)
                        } else if line.starts_with('-') && !line.starts_with("---") {
                            Style::default().fg(ratatui::style::Color::Red)
                        } else {
                            Style::default()
                        },
                    )])
                })
                .collect::<Vec<_>>(),
            commit_message,
            msg_id,
            can_apply: true,
            v_scroll_position: 0,
            v_scroll_max: diff.lines().count(),
            v_scroll_state: ratatui::widgets::ScrollbarState::default(),
            h_scroll_position: 0,
            h_scroll_max: diff.lines().map(|l| l.len()).max().unwrap_or(0),
            h_scroll_state: ratatui::widgets::ScrollbarState::default(),
        }
    }
}

impl Widget for &mut DiffReviewWidget {
    fn render(self, area: ratatui::layout::Rect, buf: &mut ratatui::buffer::Buffer) {
        let area = centered(self.h_scroll_max, self.lines.len(), area);

        self.v_scroll_position = self
            .v_scroll_position
            .min(self.v_scroll_max.saturating_sub(area.height as usize - 2));
        self.h_scroll_position = self
            .h_scroll_position
            .min(self.h_scroll_max.saturating_sub(area.width as usize - 2));

        // Only extract the lines in frame to speed up rendering
        // (otherwise Paragraph::render_text spends a bunch of time computing line lenghts for offscreen lines)
        let paragraph = Paragraph::new(
            (&self.lines[self.v_scroll_position
                ..(self.v_scroll_position + area.height as usize).min(self.lines.len())])
                .iter()
                .map(OwnedLine::as_line)
                .collect::<Vec<_>>(),
        )
        .block(Block::bordered().title(vec![
            " Review Diff ".into(),
            if self.can_apply {
                Span::styled("(y to commit, n to revert) ", ratatui::style::Color::Yellow)
            } else {
                Span::styled("(press Esc to close) ", ratatui::style::Color::Yellow)
            },
        ]))
        .scroll((0, self.h_scroll_position as u16));

        self.v_scroll_state = self
            .v_scroll_state
            .position(self.v_scroll_position)
            .content_length(self.v_scroll_max.saturating_sub(area.height as usize - 2));

        self.h_scroll_state = self
            .h_scroll_state
            .position(self.h_scroll_position)
            .content_length(self.h_scroll_max.saturating_sub(area.width as usize - 2));

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
                    .title(" Select Session ")
                    .padding(Padding::new(1, 0, 0, 0)),
            )
            .scroll((self.v_scroll_position as u16, 0));

        if self.selected_idx >= (area.height - 2) as usize + self.v_scroll_position {
            self.v_scroll_position = self.selected_idx - (area.height - 2) as usize + 1;
        } else if self.selected_idx < self.v_scroll_position {
            self.v_scroll_position = self.selected_idx;
        }
        let mut v_scroll_state = ScrollbarState::default()
            .content_length(self.sessions.len().saturating_sub(area.height as usize - 2))
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
struct ACIVizWidget {
    files: Vec<String>,
    current_idx: Option<usize>,
    contents: CodeBlock,
    in_scroll: bool,
    anim_scroll_position: usize,
    anim_scroll_time: Instant,
    target_scroll_position: usize,
    status: String,
    test_output: Option<String>,
    run_cmd_output: Option<String>,
    /// Credit usage
    usage: u64,
}

fn truncate_path(path: &str, max_len: usize) -> String {
    if path.len() <= max_len {
        return path.to_string();
    }

    let components: Vec<&str> = path.split('/').collect();
    let mut result = components.join("/");
    let mut start_idx = 0;

    while result.len() > max_len && start_idx < components.len() - 1 {
        start_idx += 1;
        result = format!(".../{}", components[start_idx..].join("/"));
    }

    result
}

impl Widget for &mut ACIVizWidget {
    fn render(self, area: ratatui::layout::Rect, buf: &mut ratatui::buffer::Buffer) {
        let area = area.inner(Margin::new(4, 4));
        Clear.render(area, buf);

        let block = Block::bordered().title(format!(
            " Bismuth Agent (${}.{:02} Used) ",
            self.usage / 100,
            self.usage % 100
        ));
        let block_area = area;
        let area = block.inner(area);
        block.render(block_area, buf);

        let vertical = ratatui::layout::Layout::vertical([
            ratatui::layout::Constraint::Length(1), // file tabs
            ratatui::layout::Constraint::Min(0),
            ratatui::layout::Constraint::Length(2), // status bar + divider
        ]);
        let [tab_area, file_area, status_area] = vertical.areas(area);

        let mut tabs = Tabs::new(
            self.files
                .iter()
                .map(|file| format!(" {} ", truncate_path(file, 36)))
                .collect::<Vec<_>>(),
        )
        .divider("")
        .padding("", "")
        .style(Style::default().bg(ratatui::style::Color::DarkGray))
        .highlight_style(Style::default().bg(ratatui::style::Color::DarkGray));
        if let Some(idx) = self.current_idx {
            tabs = tabs
                .select(idx)
                .highlight_style(Style::default().bg(ratatui::style::Color::Blue));
        }
        tabs.render(tab_area, buf);

        let lines = self.contents.lines();
        let scroll_max = lines.len().saturating_sub(file_area.height as usize);

        if self.anim_scroll_time.elapsed() > Duration::from_millis(1000) {
            if self.in_scroll {
                self.anim_scroll_position += file_area.height as usize - 5;
            }
            self.anim_scroll_time = Instant::now();
        }
        self.anim_scroll_position = self.anim_scroll_position.min(scroll_max);

        let paragraph = Paragraph::new(lines.iter().map(OwnedLine::as_line).collect::<Vec<_>>())
            .scroll((self.anim_scroll_position as u16, 0))
            .wrap(ratatui::widgets::Wrap { trim: false });

        let mut scroll_state = ratatui::widgets::ScrollbarState::default()
            .position(self.anim_scroll_position)
            .content_length(scroll_max);

        let file_scroll = Scrollbar::new(ratatui::widgets::ScrollbarOrientation::VerticalRight)
            .style(Style::default().fg(ratatui::style::Color::DarkGray));

        if let Some(test_output) = &self.test_output {
            let vertical = ratatui::layout::Layout::vertical([
                ratatui::layout::Constraint::Percentage(50),
                ratatui::layout::Constraint::Percentage(50),
            ]);
            let [file_area, test_area] = vertical.areas(file_area);

            let test_output = test_output
                .lines()
                .map(|l| Line::raw(l.replace("\t", "    ")))
                .collect::<Vec<_>>();
            let test_len = test_output.len();
            let test_paragraph = Paragraph::new(test_output)
                .block(Block::new().borders(Borders::TOP).title("─ Test Output "))
                .scroll((test_len.saturating_sub(test_area.height as usize) as u16, 0))
                .wrap(ratatui::widgets::Wrap { trim: false });

            paragraph.render(file_area, buf);
            StatefulWidget::render(file_scroll, file_area, buf, &mut scroll_state);

            let mut test_scroll_state = ratatui::widgets::ScrollbarState::default()
                .position(test_len.saturating_sub(test_area.height as usize))
                .content_length(test_len.saturating_sub(test_area.height as usize));

            let test_scroll = Scrollbar::new(ratatui::widgets::ScrollbarOrientation::VerticalRight)
                .style(Style::default().fg(ratatui::style::Color::DarkGray));

            test_paragraph.render(test_area, buf);
            StatefulWidget::render(test_scroll, test_area, buf, &mut test_scroll_state);
        } else if let Some(run_cmd_output) = &self.run_cmd_output {
            let vertical = ratatui::layout::Layout::vertical([
                ratatui::layout::Constraint::Percentage(50),
                ratatui::layout::Constraint::Percentage(50),
            ]);
            let [file_area, test_area] = vertical.areas(file_area);

            let test_output = run_cmd_output
                .lines()
                .map(|l| Line::raw(l.replace("\t", "    ")))
                .collect::<Vec<_>>();
            let test_len = test_output.len();
            let test_paragraph = Paragraph::new(test_output)
                .block(
                    Block::new()
                        .borders(Borders::TOP)
                        .title("─ Command Output "),
                )
                .scroll((test_len.saturating_sub(test_area.height as usize) as u16, 0))
                .wrap(ratatui::widgets::Wrap { trim: false });

            paragraph.render(file_area, buf);
            StatefulWidget::render(file_scroll, file_area, buf, &mut scroll_state);

            test_paragraph.render(test_area, buf);
        } else {
            paragraph.render(file_area, buf);
            StatefulWidget::render(file_scroll, file_area, buf, &mut scroll_state);
        }

        Paragraph::new(Line::styled(
            format!(" Agent Status: {} ", &self.status),
            Style::default().fg(ratatui::style::Color::Green),
        ))
        .block(Block::new().borders(Borders::TOP))
        .render(status_area, buf);
    }
}

#[derive(Clone, Debug)]
enum AppState {
    Chat,
    TerminalReset,
    SelectSession(SelectSessionWidget),
    Popup(String, String),
    ReviewDiff(DiffReviewWidget),
    // Sort of a hacky way to feed state from the event input loop back up
    ChangeSession(api::ChatSession),
    SwitchMode,
    ACI(ACIVizWidget),
    Exit,
}

struct App {
    repo_path: PathBuf,
    user: api::User,

    /// Chat is always present in the background so this is not kept in the state
    chat_history: ChatHistoryWidget,

    /// File browser is kept on the left at all times so stored here
    file_browser: FileTreeWidget,

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
    session: Arc<RwLock<api::ChatSession>>,
    state: Arc<Mutex<AppState>>,
}

fn is_in_area(mouse: &ratatui::crossterm::event::MouseEvent, area: Rect) -> bool {
    mouse.column >= area.x
        && mouse.column < area.x + area.width
        && mouse.row >= area.y
        && mouse.row < area.y + area.height
}

impl App {
    async fn new(
        repo_path: &Path,
        project: &api::Project,
        feature: &api::Feature,
        session: Arc<RwLock<api::ChatSession>>,
        current_user: &api::User,
        sessions: Vec<api::ChatSession>,
        ws_stream: tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        client: &APIClient,
    ) -> Result<Self> {
        let chat_history: Vec<ChatMessage> = client
            .get(&format!(
                "/projects/{}/features/{}/chat/sessions/{}/list",
                project.id,
                feature.id,
                session.read().unwrap().id
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

        let files = list_all_files(repo_path).unwrap();
        let session_ref = session.read().unwrap();

        trace!("NEW_APP_SESSION: {:?}", session_ref.pinned_files());
        let file_browser = FileTreeWidget::new(files, session_ref.pinned_files());

        // file_browser.pin_files(&session_ref.pinned_files());

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
                feature: feature.clone(),
                project: project.clone(),
            },
            file_browser: file_browser,
            input: tui_textarea::TextArea::default(),
            client: client.clone(),
            ws_stream: Some(ws_stream),
            project: project.clone(),
            feature: feature.clone(),
            session: session.clone(),
            state: Arc::new(Mutex::new(AppState::Chat)),
        };
        x.clear_input();
        Ok(x)
    }

    fn clear_input(&mut self) {
        self.input = tui_textarea::TextArea::default();
        self.input.set_block(Block::bordered().title(" Message "));
        self.input
            .set_placeholder_text(" Use Alt/Option + Enter to add a newline");
        self.input.set_cursor_line_style(Style::default());
    }

    async fn read_loop(
        read: &mut SplitStream<WebSocketStream<MaybeTlsStream<TcpStream>>>,
        write: &mpsc::UnboundedSender<tokio_tungstenite::tungstenite::Message>,
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
            let message_txt = &message.into_text().unwrap();

            let data: api::ws::Message = match serde_json::from_str(message_txt) {
                Ok(data) => data,
                Err(e) => {
                    debug!("Message deserialization error: {}", e);
                    continue;
                }
            };

            // Daneel snapshot resumption
            match data {
                api::ws::Message::SwitchModeResponse => (),
                api::ws::Message::PinFileResponse => (),
                _ => {
                    let mut scrollback = scrollback.lock().unwrap();
                    if scrollback.len() == 0 {
                        scrollback.push(ChatMessage::new(ChatMessageUser::AI, ""));
                    }
                }
            }

            trace!("Received message: {:#?}", data);
            match data {
                api::ws::Message::Chat(api::ws::ChatMessage { message, .. }) => {
                    let stuff: api::ws::ChatMessageBody = serde_json::from_str(&message).unwrap();
                    match stuff {
                        api::ws::ChatMessageBody::StreamingToken { token, .. } => {
                            let mut scrollback = scrollback.lock().unwrap();
                            let last_msg = scrollback.last_mut().unwrap();
                            while let Some(MessageBlock::Thinking(_)) = last_msg.blocks.last() {
                                last_msg.blocks.pop();
                            }
                            last_msg.append(&token.text);
                        }
                        api::ws::ChatMessageBody::PartialMessage { partial_message } => {
                            let mut scrollback = scrollback.lock().unwrap();
                            let msg = ChatMessage::new(ChatMessageUser::AI, &partial_message);
                            let last = scrollback.last_mut().unwrap();
                            *last = msg;
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

                            revert(repo_path).unwrap();
                            if let Some(diff) =
                                process_chat_message(repo_path, &output_modified_files).unwrap()
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
                    let repo_path = repo_path.to_path_buf();
                    let timeout = Duration::from_secs(
                        bismuth_toml::parse_config(&repo_path)
                            .unwrap_or_default()
                            .chat
                            .command_timeout,
                    );

                    process_chat_message(&repo_path, &cmd.output_modified_files)?;

                    let write_ = write.clone();
                    let state_ = state.clone();
                    tokio::spawn(async move {
                        let mut output = String::new();
                        {
                            let mut state = state_.lock().unwrap();
                            if let AppState::ACI(ref mut widget) = &mut *state {
                                widget.status = format!("Running command '{}'", cmd.command);
                                widget.run_cmd_output = Some(output.clone());
                                widget.in_scroll = false;
                            }
                        }
                        let proc_future = async {
                            let mut proc = if cfg!(not(target_os = "windows")) {
                                tokio::process::Command::new("sh")
                                    .arg("-c")
                                    .arg(&cmd.command)
                                    .stdin(std::process::Stdio::null())
                                    .stdout(std::process::Stdio::piped())
                                    .stderr(std::process::Stdio::piped())
                                    .current_dir(&repo_path)
                                    .env("TERM", "dumb")
                                    .spawn()
                                    .unwrap()
                            } else {
                                tokio::process::Command::new("cmd")
                                    .arg("/C")
                                    .arg(&cmd.command)
                                    .stdin(std::process::Stdio::null())
                                    .stdout(std::process::Stdio::piped())
                                    .stderr(std::process::Stdio::piped())
                                    .current_dir(&repo_path)
                                    .env("TERM", "dumb")
                                    .spawn()
                                    .unwrap()
                            };

                            let stdout = LinesStream::new(
                                tokio::io::BufReader::new(proc.stdout.take().unwrap()).lines(),
                            );
                            let stderr = LinesStream::new(
                                tokio::io::BufReader::new(proc.stderr.take().unwrap()).lines(),
                            );
                            let mut merged_stream = tokio_stream::StreamExt::merge(stdout, stderr);
                            while let Some(line) = merged_stream.next().await {
                                output += &line.unwrap();
                                output += "\n";
                                {
                                    let mut state = state_.lock().unwrap();
                                    if let AppState::ACI(ref mut widget) = &mut *state {
                                        widget.run_cmd_output = Some(output.clone());
                                    }
                                }
                            }

                            proc.wait().await.unwrap()
                        };

                        let exit_code = match tokio::time::timeout(timeout, proc_future).await {
                            Ok(exit_status) => exit_status.code().unwrap(),
                            Err(_) => {
                                output += "\n(Command timed out)";
                                1
                            }
                        };
                        {
                            let mut state = state_.lock().unwrap();
                            if let AppState::ACI(ref mut widget) = &mut *state {
                                widget.run_cmd_output = Some(output.clone());
                            }
                        }
                        let _ = write_
                            .send(Message::Text(
                                serde_json::to_string(&api::ws::Message::RunCommandResponse(
                                    RunCommandResponse {
                                        exit_code,
                                        output,
                                        modified_files: command_modified_files(&repo_path).unwrap(),
                                    },
                                ))
                                .unwrap(),
                            ))
                            .unwrap();
                    });
                }
                api::ws::Message::ACI(aci) => {
                    if let api::ws::ACIMessage::Start {
                        files,
                        active_file,
                        new_contents,
                        scroll_position,
                    } = aci
                    {
                        let current_idx = files.iter().position(|f| *f == active_file).unwrap();
                        let mut state = state.lock().unwrap();
                        *state = AppState::ACI(ACIVizWidget {
                            files,
                            current_idx: Some(current_idx),
                            contents: CodeBlock::new(Some(&active_file), None, &new_contents),
                            in_scroll: true,
                            anim_scroll_position: scroll_position,
                            anim_scroll_time: Instant::now(),
                            target_scroll_position: scroll_position,
                            status: format!("Looking through {}", active_file),
                            test_output: None,
                            run_cmd_output: None,
                            usage: 0,
                        });
                    } else {
                        let mut state = state.lock().unwrap();
                        if let AppState::ACI(ref mut widget) = &mut *state {
                            widget.contents.diff_highlight_lines = None;
                            widget.in_scroll = false;
                            widget.test_output = None; // clear test output after next action
                            widget.run_cmd_output = None; // ditto
                            match aci {
                                api::ws::ACIMessage::Start { .. } => {
                                    return Err(anyhow!(
                                        "Received ACI start message but already in ACI state"
                                    ));
                                }
                                api::ws::ACIMessage::Scroll {
                                    status,
                                    scroll_position,
                                } => {
                                    widget.in_scroll = true;
                                    widget.target_scroll_position = scroll_position;
                                    widget.anim_scroll_time = Instant::now();
                                    widget.status = status;
                                }
                                api::ws::ACIMessage::Switch {
                                    status,
                                    active_file,
                                    new_contents,
                                    scroll_position,
                                } => {
                                    widget.contents =
                                        CodeBlock::new(Some(&active_file), None, &new_contents);
                                    widget.anim_scroll_position = scroll_position;
                                    widget.target_scroll_position = scroll_position;
                                    widget.anim_scroll_time = Instant::now();
                                    widget.in_scroll = true;
                                    widget.status = status;
                                    if let Some(current_idx) =
                                        widget.files.iter().position(|f| *f == active_file)
                                    {
                                        widget.current_idx = Some(current_idx);
                                    } else {
                                        widget.files.push(active_file);
                                        widget.current_idx = Some(widget.files.len() - 1);
                                    }
                                }
                                api::ws::ACIMessage::Close { status } => {
                                    widget.status = status;
                                    widget.files.remove(widget.current_idx.unwrap());
                                    widget.current_idx = None;
                                    widget.contents = CodeBlock::new(None, None, "");
                                }
                                api::ws::ACIMessage::Create {
                                    status,
                                    active_file,
                                    new_contents,
                                    files,
                                    scroll_position,
                                } => {
                                    widget.files = files;
                                    widget.contents =
                                        CodeBlock::new(Some(&active_file), None, &new_contents);
                                    widget.anim_scroll_position = scroll_position;
                                    widget.target_scroll_position = scroll_position;
                                    widget.anim_scroll_time = Instant::now();
                                    widget.in_scroll = true;
                                    widget.status = status;
                                    widget.current_idx = Some(
                                        widget
                                            .files
                                            .iter()
                                            .position(|f| *f == active_file)
                                            .unwrap(),
                                    );
                                }
                                api::ws::ACIMessage::Edit {
                                    status,
                                    new_contents,
                                    scroll_position,
                                    changed_range,
                                } => {
                                    widget.contents = CodeBlock::new(
                                        Some(&widget.files[widget.current_idx.unwrap()]),
                                        None,
                                        &new_contents,
                                    );
                                    widget.contents.diff_highlight_lines =
                                        Some((changed_range.0..changed_range.1).collect());
                                    widget.anim_scroll_position = scroll_position.saturating_sub(5);
                                    widget.target_scroll_position =
                                        scroll_position.saturating_sub(5);
                                    widget.status = status;
                                }
                                api::ws::ACIMessage::Test {
                                    status,
                                    test_output,
                                } => {
                                    widget.test_output = Some(test_output.replace("\t", "    "));
                                    widget.status = status;
                                }
                                api::ws::ACIMessage::Status { status } => {
                                    widget.status = status;
                                }
                                api::ws::ACIMessage::End => {
                                    // Fully reset terminal before going back to chat to clear any corruption from running commands
                                    *state = AppState::TerminalReset;
                                }
                            }
                        }
                    }
                }
                api::ws::Message::FileRPC(req) => {
                    let resp = match req {
                        api::ws::FileRPCRequest::List => {
                            let files = list_all_files(repo_path).unwrap();
                            api::ws::FileRPCResponse::List { files }
                        }
                        api::ws::FileRPCRequest::Read { path } => {
                            let content = std::fs::read_to_string(repo_path.join(&path)).ok();
                            api::ws::FileRPCResponse::Read { content }
                        }
                        api::ws::FileRPCRequest::Search { query } => {
                            let results = list_all_files(repo_path)
                                .unwrap()
                                .into_iter()
                                .flat_map(|file| {
                                    if let Ok(contents) =
                                        std::fs::read_to_string(repo_path.join(&file))
                                    {
                                        contents
                                            .lines()
                                            .enumerate()
                                            .filter_map(|(line, text)| {
                                                if text.contains(&query) {
                                                    Some((file.clone(), line + 1, text.to_string()))
                                                } else {
                                                    None
                                                }
                                            })
                                            .collect::<Vec<_>>()
                                    } else {
                                        vec![]
                                    }
                                })
                                .collect();
                            api::ws::FileRPCResponse::Search { results }
                        }
                    };
                    write
                        .send(Message::Text(
                            serde_json::to_string(&api::ws::Message::FileRPCResponse(resp))
                                .unwrap(),
                        ))
                        .unwrap();
                }
                api::ws::Message::Error(err) => {
                    return Err(anyhow!(err));
                }
                api::ws::Message::Usage(usage) => {
                    let mut state = state.lock().unwrap();
                    if let AppState::ACI(ref mut widget) = &mut *state {
                        widget.usage = usage;
                    }
                }
                api::ws::Message::SwitchModeResponse => {
                    let mut state = state.lock().unwrap();
                    *state = AppState::SwitchMode;
                }
                _ => {}
            }
        }
    }

    fn calculate_layout(&self, frame_area: Rect, input_lines: usize) -> (Rect, Rect, Rect) {
        let horizontal = ratatui::layout::Layout::horizontal([
            ratatui::layout::Constraint::Percentage(20),
            ratatui::layout::Constraint::Percentage(80),
        ]);
        let [file_browser_area, chat_area] = horizontal.areas(frame_area);

        let vertical = ratatui::layout::Layout::vertical([
            ratatui::layout::Constraint::Percentage(100),
            ratatui::layout::Constraint::Min((input_lines.clamp(1, 3) + 2) as u16),
        ]);
        let [history_area, input_area] = vertical.areas(chat_area);

        (file_browser_area, history_area, input_area)
    }

    async fn run(
        &mut self,
        terminal: &mut ratatui::Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>,
    ) -> Result<Option<api::ChatSession>> {
        terminal.clear()?; // needed to clear session selection screen

        let (mut write_sink, mut read) = self.ws_stream.take().unwrap().split();
        let (dead_tx, mut dead_rx) = tokio::sync::oneshot::channel();

        let (write, mut write_source) = mpsc::unbounded_channel();

        let write_arc = Arc::new(write.clone());
        let write_arc_clone = write_arc.clone();

        self.file_browser
            .set_leaf_callback(move |path: &str, node: SimpleTreeNode| {
                write_arc_clone
                    .send(Message::Text(
                        serde_json::to_string(&api::ws::Message::PinFile(
                            api::ws::PinFileMessage {
                                path: path.to_string(),
                            },
                        ))
                        .unwrap(),
                    ))
                    .unwrap();

                match node.style {
                    TreeNodeStyle::Pinned => TreeNodeStyle::Default,
                    _ => TreeNodeStyle::Pinned,
                }
            });

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
        loop {
            let state = { self.state.lock().unwrap().clone() };
            if let AppState::Exit = state {
                return Ok(None);
            }
            if let AppState::ChangeSession(new_session) = state {
                return Ok(Some(new_session));
            }
            if let AppState::SwitchMode = state {
                let mut session_write_ref = self.session.write().unwrap();
                session_write_ref.swap_mode();

                let mut state = self.state.lock().unwrap();
                *state = AppState::Chat;
            }
            if let AppState::TerminalReset = state {
                terminal.clear()?;
                let mut state = self.state.lock().unwrap();
                *state = AppState::Chat;
                continue;
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
                        &mut self.file_browser,
                        &self.input,
                    )
                })?;
            }

            if !tokio::task::spawn_blocking(move || event::poll(Duration::from_millis(40)))
                .await??
            {
                continue;
            }

            match state {
                AppState::Exit => {
                    return Ok(None);
                }
                AppState::ChangeSession(new_session) => {
                    return Ok(Some(new_session));
                }
                // Handled before event polling
                AppState::SwitchMode => {}
                AppState::ReviewDiff(diff) => match event::read()? {
                    Event::Key(key) if key.kind == event::KeyEventKind::Press => match key.code {
                        KeyCode::Char('y') if diff.can_apply => {
                            commit(&self.repo_path, diff.commit_message.as_deref())?;
                            let client = self.client.clone();
                            let project = self.project.id;
                            let feature = self.feature.id;
                            let message_id = diff.msg_id;
                            let paths = list_all_files(&self.repo_path).unwrap();
                            let current_pinned =
                                self.file_browser.pinned.clone().into_iter().collect();
                            self.file_browser = FileTreeWidget::new(paths, current_pinned);

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
                        KeyCode::Char('n') if diff.can_apply => {
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
                        KeyCode::Esc if !diff.can_apply => {
                            let mut state = self.state.lock().unwrap();
                            *state = AppState::Chat;
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
                        KeyCode::PageUp => {
                            let mut state = self.state.lock().unwrap();
                            if let AppState::ReviewDiff(diff_widget) = &mut *state {
                                diff_widget.v_scroll_position = diff_widget
                                    .v_scroll_position
                                    .saturating_sub(10)
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
                        KeyCode::Char(' ') | KeyCode::PageDown => {
                            let mut state = self.state.lock().unwrap();
                            if let AppState::ReviewDiff(diff_widget) = &mut *state {
                                diff_widget.v_scroll_position = diff_widget
                                    .v_scroll_position
                                    .saturating_add(10)
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
                AppState::Chat => {
                    let last_generation_done = self
                        .chat_history
                        .messages
                        .lock()
                        .unwrap()
                        .last()
                        .map_or(true, |msg| msg.finalized);
                    if !last_generation_done {
                        if let Event::Key(key) = event::read()? {
                            if key.kind == event::KeyEventKind::Press
                                && key.modifiers.contains(event::KeyModifiers::CONTROL)
                                && key.code == KeyCode::Char('c')
                            {
                                write
                                    .send(Message::Text(serde_json::to_string(
                                        &api::ws::Message::KillGeneration,
                                    )?))
                                    .unwrap()
                            }
                        }
                    } else {
                        let next_event = event::read()?;

                        match next_event {
                            Event::Mouse(mouse) => {
                                let terminal_rect = Rect::new(
                                    0,
                                    0,
                                    terminal.size()?.width,
                                    terminal.size()?.height,
                                );
                                let (fb_area, _, _) = self.calculate_layout(
                                    terminal_rect,
                                    (&self.input.lines().len().clamp(1, 3) + 2) as usize,
                                );

                                if is_in_area(&mouse, fb_area) {
                                    self.file_browser.handle_event(&next_event, fb_area);
                                } else {
                                    match mouse.kind {
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
                                        event::MouseEventKind::Up(MouseButton::Left) => {
                                            let mut messages =
                                                self.chat_history.messages.lock().unwrap();

                                            if let Ok(mut clipboard_ctx) =
                                                copypasta::ClipboardContext::new()
                                            {
                                                for ((start, _end), block) in self
                                                    .chat_history
                                                    .message_hitboxes
                                                    .iter()
                                                    .zip(messages.iter())
                                                {
                                                    // -1 for the border of chat history
                                                    if (*start as isize
                                                        - self.chat_history.scroll_position
                                                            as isize)
                                                        == (mouse.row as isize) - 1
                                                        && (mouse.column == fb_area.width + 1
                                                            || mouse.column == fb_area.width + 2)
                                                    {
                                                        clipboard_ctx
                                                            .set_contents(block.raw.clone())
                                                            .unwrap();
                                                    }
                                                }
                                            }

                                            let mut hitboxes_iter =
                                                self.chat_history.code_block_hitboxes.iter();
                                            for msg in messages.iter_mut() {
                                                for block in &mut msg.blocks {
                                                    if let MessageBlock::Code(code) = block {
                                                        let (start, end) =
                                                            hitboxes_iter.next().unwrap();
                                                        // -1 for the border of chat history
                                                        if (*start as isize
                                                            - self.chat_history.scroll_position
                                                                as isize)
                                                            < (mouse.row as isize)
                                                            && (*end as isize
                                                                - self.chat_history.scroll_position
                                                                    as isize)
                                                                > (mouse.row as isize) - 1
                                                        {
                                                            code.folded = !code.folded;
                                                            msg.block_line_cache.1.clear();
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                        _ => {}
                                    }
                                }
                            }
                            Event::Key(key) if key.kind == event::KeyEventKind::Press => {
                                match key.code {
                                    KeyCode::Char('c')
                                        if key.modifiers.contains(event::KeyModifiers::CONTROL) =>
                                    {
                                        let mut state = self.state.lock().unwrap();
                                        *state = AppState::Exit;
                                    }
                                    KeyCode::Char('m')
                                        if key.modifiers.contains(event::KeyModifiers::CONTROL) =>
                                    {
                                        write
                                            .send(Message::Text(serde_json::to_string(
                                                &api::ws::Message::SwitchMode,
                                            )?))
                                            .unwrap();
                                    }
                                    KeyCode::Char('n')
                                        if key.modifiers.contains(event::KeyModifiers::CONTROL) =>
                                    {
                                        let session = self
                                            .client
                                            .post(&format!(
                                                "/projects/{}/features/{}/chat/sessions",
                                                self.project.id, self.feature.id
                                            ))
                                            .json(&json!({ "name": None::<&str> }))
                                            .send()
                                            .await?
                                            .error_body_for_status()
                                            .await?
                                            .json()
                                            .await?;
                                        let mut state = self.state.lock().unwrap();

                                        *state = AppState::ChangeSession(session);
                                    }
                                    KeyCode::Enter => {
                                        // ALT+enter for manual newlines
                                        if key.modifiers.contains(event::KeyModifiers::ALT)
                                            || key.modifiers.contains(event::KeyModifiers::SHIFT)
                                        {
                                            self.input.input(key);
                                        } else {
                                            self.handle_chat_input(&write).await?;
                                            self.chat_history.scroll_position =
                                                self.chat_history.scroll_max;
                                        }
                                    }
                                    _ => {
                                        self.input.input(key);
                                    }
                                }
                            }
                            Event::Paste(paste) => {
                                self.input.insert_str(paste);
                            }
                            _ => (),
                        }
                    }
                }
                AppState::ACI(_) => match event::read()? {
                    Event::Key(key) if key.kind == event::KeyEventKind::Press => match key.code {
                        KeyCode::Char('c')
                            if key.modifiers.contains(event::KeyModifiers::CONTROL) =>
                        {
                            write
                                .send(Message::Text(serde_json::to_string(
                                    &api::ws::Message::KillGeneration,
                                )?))
                                .unwrap();
                            let mut state = self.state.lock().unwrap();
                            *state = AppState::Chat;
                        }
                        _ => {}
                    },
                    _ => {}
                },
                // Handled before event polling
                AppState::TerminalReset => {}
            }
        }
    }

    async fn handle_chat_input(
        &mut self,
        write: &mpsc::UnboundedSender<tokio_tungstenite::tungstenite::Message>,
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
                    "/mode" | "/m" => {
                        write
                            .send(Message::Text(serde_json::to_string(
                                &api::ws::Message::SwitchMode,
                            )?))
                            .unwrap();
                    }
                    "/pin" => {}
                    "/help" => {
                        *state = AppState::Popup(
                            "Help".to_string(),
                            r#"/exit, /quit, or Ctrl+C: Exit the chat
/docs: Open the Bismuth documentation
/new-session [NAME]: Start a new session
/rename-session <NAME>: Rename the current session
/session [NAME]: Switch to a different session
/feedback <DESCRIPTION>: Send us feedback
/diff: Review the last diff Bismuth made
/quit or Ctrl+C: Exit the chat
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
                                        self.project.id,
                                        self.feature.id,
                                        self.session.read().unwrap().id
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
                                if self.chat_history.sessions.is_empty() {
                                    *state = AppState::Popup(
                                        "Error".to_string(),
                                        "\n\n    There are no other sessions    \n\n".to_string(),
                                    );
                                } else {
                                    *state = AppState::SelectSession(SelectSessionWidget {
                                        sessions: self.chat_history.sessions.clone(),
                                        selected_idx: 0,
                                        v_scroll_position: 0,
                                    })
                                }
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
                        let last = repo.revparse_single("HEAD")?;
                        if last.peel_to_commit()?.author().name().unwrap() == BISMUTH_AUTHOR {
                            repo.reset(
                                &repo.revparse_single("HEAD~1")?,
                                git2::ResetType::Hard,
                                Some(git2::build::CheckoutBuilder::new().force()),
                            )?;
                        }
                    }
                    "/diff" => {
                        let repo = git2::Repository::open(&self.repo_path)?;
                        let last = repo.revparse_single("HEAD")?;
                        if last.peel_to_commit()?.author().name().unwrap() == BISMUTH_AUTHOR {
                            let mut widget = DiffReviewWidget::new(
                                Command::new("git")
                                    .arg("-C")
                                    .arg(&self.repo_path)
                                    .arg("--no-pager")
                                    .arg("diff")
                                    .arg("HEAD~1..HEAD")
                                    .output()
                                    .map_err(|e| anyhow!("Failed to run git diff: {}", e))
                                    .and_then(|o| {
                                        if o.status.success() {
                                            Ok(o.stdout)
                                        } else {
                                            Err(anyhow!("git diff failed (code={})", o.status))
                                        }
                                    })
                                    .and_then(|s| String::from_utf8(s).map_err(|e| anyhow!(e)))?,
                                0,
                                None,
                            );
                            widget.can_apply = false;
                            *state = AppState::ReviewDiff(widget);
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

            let modified_files = if self.project.has_pushed {
                list_changed_files(&self.repo_path)?
                    .into_iter()
                    .map(|path| {
                        let content = std::fs::read_to_string(self.repo_path.join(&path))
                            .unwrap_or("".to_string());
                        api::ws::ChatModifiedFile {
                            name: path.file_name().unwrap().to_str().unwrap().to_string(),
                            project_path: path.to_str().unwrap().to_string(),
                            content,
                            deleted: Some(!self.repo_path.join(&path).exists()),
                        }
                    })
                    .collect()
            } else {
                vec![]
            };

            write
                .send(Message::Text(serde_json::to_string(
                    &api::ws::Message::Chat(api::ws::ChatMessage {
                        message: input.clone(),
                        modified_files,
                        request_type_analysis: false,
                    }),
                )?))
                .unwrap();
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
    daneel_url: &str,
) -> Result<()> {
    let repo_path = repo_path.to_path_buf();

    if project.has_pushed
        && list_changed_files(&repo_path)?
            .into_iter()
            .map(|path| {
                std::fs::metadata(&repo_path.join(&path))
                    .map(|s| s.len())
                    .unwrap_or(0)
            })
            .sum::<u64>()
            > 8 * 1024 * 1024
    {
        return Err(anyhow!(
            "There are too many unpushed changes. Please commit, `git push bismuth`, and try again."
        ));
    }

    let mut session = Arc::new(RwLock::new(session.clone()));
    let mut terminal = terminal::init()?;

    let status = loop {
        let (mut ws_stream, _) = connect_async(daneel_url).await.expect("Failed to connect");

        ws_stream
            .send(Message::Text(serde_json::to_string(
                &api::ws::Message::Auth(api::ws::AuthMessage {
                    feature_id: feature.id,
                    session_id: session.read().unwrap().id,
                    token: client.token.clone(),
                }),
            )?))
            .await?;

        debug!("Connected to chat");

        let mut app = App::new(
            &repo_path,
            project,
            feature,
            session,
            current_user,
            sessions.clone(),
            ws_stream,
            client,
        )
        .await?;

        let status = app.run(&mut terminal).await;
        match status {
            Ok(Some(new_session)) => {
                session = Arc::new(RwLock::new(new_session));
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
    file_browser: &mut FileTreeWidget,
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

    // First split into columns - left for file browser, right for chat
    let horizontal = ratatui::layout::Layout::horizontal([
        ratatui::layout::Constraint::Percentage(20), // file browser width, adjust as needed
        ratatui::layout::Constraint::Percentage(80), // chat area width
    ]);
    let [file_browser_area, chat_area] = horizontal.areas(frame.area());

    // Then split the chat area vertically like you already had
    let vertical = ratatui::layout::Layout::vertical([
        ratatui::layout::Constraint::Percentage(100),
        ratatui::layout::Constraint::Min((input.lines().len().clamp(1, 3) + 2) as u16),
    ]);
    let [history_area, input_area] = vertical.areas(chat_area); // Note: using chat_area instead of frame.area()

    // Render everything
    frame.render_widget(file_browser, file_browser_area);
    frame.render_widget(chat_history, history_area);
    frame.render_widget(input, input_area);

    let mut state = state.lock().unwrap();
    match &mut *state {
        AppState::ReviewDiff(diff_widget) => {
            frame.render_widget(diff_widget, frame.area());
        }
        AppState::Popup(title, text) => {
            let paragraph = Paragraph::new(text.clone()).block(Block::bordered().title(vec![
                format!(" {} ", title).into(),
                Span::styled("(press any key to close) ", ratatui::style::Color::Yellow),
            ]));
            let area = centered_paragraph(&paragraph, frame.area());
            frame.render_widget(Clear, area);
            frame.render_widget(paragraph, area);
        }
        AppState::SelectSession(widget) => {
            frame.render_widget(widget, frame.area());
        }
        AppState::ACI(widget) => {
            frame.render_widget(widget, frame.area());
        }
        _ => {}
    }
}

fn centered(width: usize, height: usize, r: Rect) -> Rect {
    // +2 for border
    let width = (width + 2).min(r.width as usize) as u16;
    let height = (height + 2).min(r.height as usize) as u16;

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

fn centered_paragraph(paragraph: &Paragraph, r: Rect) -> Rect {
    let width = paragraph.line_width();
    centered(width, paragraph.line_count(width as u16), r)
}

mod terminal {
    use std::{io, process::Command};

    use log::debug;
    use ratatui::{
        backend::CrosstermBackend,
        crossterm::{
            event::{
                DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste,
                EnableMouseCapture, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
                PushKeyboardEnhancementFlags,
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
            EnableBracketedPaste,
        )?;
        #[cfg(not(target_os = "windows"))]
        {
            execute!(
                io::stdout(),
                PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
            )?;
        }
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
        debug!("Restoring terminal");
        #[cfg(not(target_os = "windows"))]
        {
            if let Err(err) = execute!(io::stdout(), PopKeyboardEnhancementFlags) {
                eprintln!("error restoring terminal: {err}");
            }
        }
        if let Err(err) = execute!(
            io::stdout(),
            DisableBracketedPaste,
            DisableMouseCapture,
            LeaveAlternateScreen,
        ) {
            eprintln!("error restoring terminal: {err}");
        }
        if let Err(err) = disable_raw_mode() {
            eprintln!("error disabling raw mode: {err}");
        }
        // Reset cursor shape
        let _ = Command::new("tput").arg("cnorm").status();
    }
}

#[cfg(test)]
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
