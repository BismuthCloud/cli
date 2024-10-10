use std::{
    collections::{HashSet, VecDeque},
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{anyhow, Result};
use futures::{SinkExt, StreamExt, TryStreamExt};
use log::{debug, trace};
use ratatui::{
    crossterm::{
        cursor::SetCursorStyle,
        event::{self, Event, KeyCode, MouseButton},
    },
    layout::{Constraint, Layout, Rect},
    style::{Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Clear, Padding, Paragraph, Scrollbar, StatefulWidget, Widget},
};
use syntect::easy::HighlightLines;
use syntect::util::LinesWithEndings;
use tokio::sync::mpsc;
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};
use url::Url;

use crate::{
    api::{self, ws::ChatModifiedFile},
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
        .and_then(|s| String::from_utf8(s).map_err(|e| anyhow!(e)))?;

    Ok(Some(diff))
}

fn commit(repo_path: &Path) -> Result<()> {
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

    repo.commit(
        Some("HEAD"),
        &signature,
        &signature,
        format!("Bismuth: {}", changed_files.join(", ")).as_str(),
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

#[derive(Clone, Debug)]
struct CodeBlock {
    language: String,
    /// The syntax highlighted code
    lines: Vec<Line<'static>>,
    folded: bool,
}

impl CodeBlock {
    fn new(language: Option<&str>, raw_code: &str) -> Self {
        // TODO: BAAADDD
        let raw_code = Box::leak(raw_code.to_string().into_boxed_str());

        let ps = two_face::syntax::extra_newlines();
        let ts = two_face::theme::extra();
        let syntax = ps
            .find_syntax_by_extension(match language {
                Some("python") => "py",
                Some("markdown") => "md",
                Some("javascript") => "js",
                Some("typescript") => "tsx",
                _ => "txt",
            })
            .unwrap();
        let mut h = HighlightLines::new(
            syntax,
            ts.get(two_face::theme::EmbeddedThemeName::Base16OceanDark),
        );
        let lines = LinesWithEndings::from(raw_code)
            .map(|line| {
                Line::from(
                    h.highlight_line(line, &ps)
                        .unwrap()
                        .into_iter()
                        .map(|(syntect_style, content)| {
                            Span::styled(
                                content,
                                Style {
                                    fg: match syntect_style.foreground {
                                        // TODO: detect terminal and disable highlighting if 24 bit color is unsupported
                                        syntect::highlighting::Color { r, g, b, a } => {
                                            Some(ratatui::style::Color::Rgb(r, g, b))
                                        }
                                        _ => None,
                                    },
                                    bg: None,
                                    underline_color: None,
                                    add_modifier: ratatui::style::Modifier::empty(),
                                    sub_modifier: ratatui::style::Modifier::empty(),
                                },
                            )
                        })
                        .collect::<Vec<Span>>(),
                )
            })
            .collect();

        Self {
            language: language.unwrap_or("").to_string(),
            lines,
            folded: true,
        }
    }
}

#[derive(Clone, Debug)]
enum MessageBlock {
    Text(Vec<Line<'static>>),
    Thinking(String),
    Code(CodeBlock),
}

impl MessageBlock {
    fn new_text(text: &str) -> Self {
        let text = Box::leak(text.to_string().into_boxed_str());
        Self::Text(text.lines().map(|line| Line::raw(line)).collect::<Vec<_>>())
    }
}

#[derive(Clone, Debug)]
struct ChatMessage {
    user: ChatMessageUser,
    raw: String,
    finalized: bool,
    blocks: Vec<MessageBlock>,
}

impl ChatMessage {
    fn new(user: ChatMessageUser, content: &str) -> Self {
        let content = content
            .replace("\n<BCODE>\n", "\n")
            .replace("\n</BCODE>\n", "\n");
        let root = markdown::to_mdast(&content, &markdown::ParseOptions::default()).unwrap();
        let mut blocks: Vec<_> = match root.children() {
            Some(nodes) => nodes
                .into_iter()
                .filter_map(|block| match block {
                    markdown::mdast::Node::Code(code) => {
                        if code.value.len() > 0 {
                            Some(MessageBlock::Code(CodeBlock::new(
                                code.lang.as_deref(),
                                &code.value,
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
                            &content[position.start.offset..position.end.offset],
                        ))
                    }
                })
                .collect(),
            None => vec![],
        };

        let prefix_spans = Self::format_user(&user);

        if let Some(MessageBlock::Text(text_lines)) = blocks.first_mut() {
            text_lines[0].spans = prefix_spans
                .into_iter()
                .chain(text_lines[0].spans.drain(..))
                .collect();
        } else {
            blocks.insert(0, MessageBlock::Text(vec![Line::from(prefix_spans)]));
        }

        Self {
            user,
            raw: content.to_string(),
            finalized: false,
            blocks,
        }
    }

    fn format_user<'a>(user: &ChatMessageUser) -> Vec<Span<'a>> {
        vec![
            match user {
                ChatMessageUser::AI => ratatui::text::Span::styled(
                    "Bismuth",
                    ratatui::style::Style::default().fg(ratatui::style::Color::Magenta),
                ),
                ChatMessageUser::User(ref user) => ratatui::text::Span::styled(
                    user.clone(),
                    ratatui::style::Style::default().fg(ratatui::style::Color::Cyan),
                ),
            },
            ": ".into(),
        ]
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

    selection: Option<((usize, usize), (usize, usize))>,
}

impl Widget for &mut ChatHistoryWidget {
    fn render(self, area: ratatui::layout::Rect, buf: &mut ratatui::buffer::Buffer) {
        let block = Block::new()
            .title("Chat History")
            .borders(ratatui::widgets::Borders::ALL)
            .padding(Padding::new(1, 0, 0, 0));

        let mut line_idx = 0;
        // start,end line idxs for each code block
        let mut code_block_hitboxes: Vec<(usize, usize)> = vec![];

        let messages = self.messages.lock().unwrap();
        let lines: Vec<_> = messages
            .iter()
            .flat_map(|message| {
                let message_lines: Vec<_> = message
                    .blocks
                    .iter()
                    .flat_map(|block| {
                        let mut lines = match block {
                            MessageBlock::Text(lines) => lines.clone(),
                            MessageBlock::Thinking(detail) => {
                                vec![Line::raw(format!(
                                    "{} {}",
                                    detail,
                                    vec!['|', '\\', '-', '/'][SystemTime::now()
                                        .duration_since(UNIX_EPOCH)
                                        .unwrap()
                                        .subsec_millis()
                                        as usize
                                        / 251]
                                ))]
                            }
                            MessageBlock::Code(code) => {
                                let code_block_lines = if code.folded {
                                    vec![Line::styled(
                                        title_case(
                                            &format!(
                                                "{} code block (click to expand)",
                                                &code.language
                                            )
                                            .trim(),
                                        ),
                                        ratatui::style::Style::default()
                                            .fg(ratatui::style::Color::Yellow),
                                    )]
                                } else {
                                    code.lines
                                        .iter()
                                        .map(|line| {
                                            let mut indented = line.clone();
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
                        // have to "simulate" line wrapping here to get an accurate line count
                        line_idx += Paragraph::new(ratatui::text::Text::from_iter(lines.clone()))
                            .wrap(ratatui::widgets::Wrap { trim: false })
                            .line_count(area.width - 3); // -1 for each L/R border + 1 padding
                        lines
                    })
                    .collect();

                message_lines
            })
            .collect();

        let paragraph = Paragraph::new(ratatui::text::Text::from_iter(lines))
            .block(block)
            .scroll((self.scroll_position as u16, 0))
            .wrap(ratatui::widgets::Wrap { trim: false });

        // -2 to account for the borders + l padding
        // +3 so you can scroll past the bottom a bit to see this is really the end
        let nlines = paragraph.line_count(area.width - 3) + 3;
        let old_scroll_max = self.scroll_max;
        self.scroll_max = nlines.max(area.height as usize) - (area.height as usize);
        // Auto scroll to the bottom if we were already at the bottom
        if self.scroll_position == old_scroll_max {
            self.scroll_position = self.scroll_max;
        }
        self.scroll_state = self.scroll_state.content_length(self.scroll_max);

        self.code_block_hitboxes = code_block_hitboxes;

        paragraph.render(area, buf);
        StatefulWidget::render(
            Scrollbar::new(ratatui::widgets::ScrollbarOrientation::VerticalRight),
            area,
            buf,
            &mut self.scroll_state,
        );
    }
}

#[derive(Clone, Debug)]
struct DiffReviewWidget {
    diff: String,
    scroll_position: usize,
    scroll_max: usize,
    scroll_state: ratatui::widgets::ScrollbarState,
}

impl DiffReviewWidget {
    fn new(diff: String) -> Self {
        Self {
            diff,
            scroll_position: 0,
            scroll_max: 0,
            scroll_state: ratatui::widgets::ScrollbarState::default(),
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
            .scroll((self.scroll_position as u16, 0));

        let nlines = self.diff.lines().count();
        self.scroll_max = nlines.max(area.height as usize) - (area.height as usize);
        self.scroll_state = self.scroll_state.content_length(self.scroll_max);

        let area = centered_paragraph(&paragraph, area);
        Clear.render(area, buf);
        paragraph.render(area, buf);
        StatefulWidget::render(
            Scrollbar::new(ratatui::widgets::ScrollbarOrientation::VerticalRight),
            area,
            buf,
            &mut self.scroll_state,
        );
    }
}

#[derive(Clone, Debug)]
enum AppState {
    Chat,
    Help,
    Exit,
    ReviewDiff(DiffReviewWidget),
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
    state: Arc<Mutex<AppState>>,
    focus: bool,
}

impl App {
    fn new(
        repo_path: &Path,
        project: &api::Project,
        feature: &api::Feature,
        current_user: &api::User,
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
                selection: None,
            },
            input: tui_textarea::TextArea::default(),
            client: client.clone(),
            ws_stream: Some(ws_stream),
            project: project.clone(),
            feature: feature.clone(),
            state: Arc::new(Mutex::new(AppState::Chat)),
            focus: true,
        };
        x.clear_input();
        x
    }

    fn clear_input(&mut self) {
        self.input = tui_textarea::TextArea::default();
        self.input.set_block(Block::bordered().title("Message"));
        self.input.set_cursor_line_style(Style::default());
    }

    async fn run(&mut self) -> Result<()> {
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
            loop {
                let message = match read.try_next().await {
                    Err(_) => {
                        break;
                    }
                    Ok(None) => {
                        break;
                    }
                    Ok(Some(message)) => message,
                };
                if let Message::Ping(_) = message {
                    write_
                        .send(Message::Pong(Default::default()))
                        .await
                        .unwrap();
                    continue;
                }
                if let Message::Close(_) = message {
                    return;
                }
                let scrollback = scrollback.clone();
                let data: api::ws::Message =
                    serde_json::from_str(&message.into_text().unwrap()).unwrap();
                match data {
                    api::ws::Message::Chat(api::ws::ChatMessage { message, .. }) => {
                        let stuff: api::ws::ChatMessageBody =
                            serde_json::from_str(&message).unwrap();
                        match stuff {
                            api::ws::ChatMessageBody::StreamingToken { token, .. } => {
                                let mut scrollback = scrollback.lock().unwrap();
                                let last_msg = scrollback.last_mut().unwrap();
                                let mut new_raw = last_msg.raw.clone() + &token.text;
                                new_raw = new_raw.replace("\n<BCODE>\n", "\n");
                                *last_msg = ChatMessage::new(last_msg.user.clone(), &new_raw);
                                // let nblocks = last_msg.blocks.len();
                                // let last_block = last_msg.blocks.last_mut().unwrap();
                                // match last_block {
                                //     MessageBlock::Text(lines) => {
                                //         if lines.len() > 0 {
                                //             lines
                                //                 .last_mut();
                                //                 .unwrap()
                                //                 .spans
                                //                 .push(Span::raw(token.text));
                                //             // TODO: trim off ```python ?
                                //         } else {
                                //             lines.push(Line::from(vec![Span::raw(token.text)]));
                                //         }
                                //     }
                                //     MessageBlock::Thinking => {
                                //         if nblocks == 1 {
                                //             // Replace the entire message so we get the Bismuth: prefix back
                                //             *last_msg =
                                //                 ChatMessage::new(ChatMessageUser::AI, &token.text);
                                //         } else {
                                //             // Otherwise just replace this thinking block
                                //             *last_block = MessageBlock::new_text(&token.text);
                                //         }
                                //     }
                                //     _ => {
                                //         last_msg.blocks.push(MessageBlock::new_text(&token.text));
                                //     }
                                // }
                            }
                            api::ws::ChatMessageBody::PartialMessage { partial_message } => {
                                let partial_message = partial_message
                                    .replace("\n<BCODE>\n", "\n")
                                    .replace("\n</BCODE>\n", "\n");
                                let mut scrollback = scrollback.lock().unwrap();
                                let last = scrollback.last_mut().unwrap();
                                *last = ChatMessage::new(ChatMessageUser::AI, &partial_message);
                            }
                            api::ws::ChatMessageBody::FinalizedMessage {
                                generated_text,
                                output_modified_files,
                                ..
                            } => {
                                {
                                    let mut scrollback = scrollback.lock().unwrap();
                                    let last = scrollback.last_mut().unwrap();
                                    let generated_text = generated_text
                                        .replace("\n<BCODE>\n", "\n")
                                        .replace("\n</BCODE>\n", "\n");
                                    *last = ChatMessage::new(ChatMessageUser::AI, &generated_text);
                                    last.finalized = true;
                                }

                                if let Some(diff) =
                                    process_chat_message(&repo_path, &output_modified_files)
                                        .unwrap()
                                {
                                    if !diff.is_empty() {
                                        let mut state = state.lock().unwrap();
                                        *state = AppState::ReviewDiff(DiffReviewWidget::new(diff));
                                    }
                                }
                            }
                        }
                    }
                    api::ws::Message::ResponseState(resp) => {
                        let mut scrollback = scrollback.lock().unwrap();
                        let last = scrollback.last_mut().unwrap();
                        // sorta hacky, but if the last block is code when we start thinking
                        // remove it as it's a small partial message
                        if let Some(MessageBlock::Code(_)) = last.blocks.last() {
                            last.blocks.pop();
                        }
                        if let Some(MessageBlock::Thinking(_)) = last.blocks.last() {
                            *last.blocks.last_mut().unwrap() =
                                MessageBlock::Thinking(resp.state.clone());
                        } else {
                            last.blocks.push(MessageBlock::Thinking(resp.state.clone()));
                        }
                    }
                    _ => {}
                }
            }
            dead_tx.send(()).unwrap();
        });

        let mut last_draw = Instant::now();
        let mut last_input = Instant::now();
        let mut input_delay = VecDeque::new();
        loop {
            let state = { self.state.lock().unwrap().clone() };
            if let AppState::Exit = state {
                return Ok(());
            }
            if dead_rx.try_recv().is_ok() {
                return Err(anyhow!("Chat connection closed"));
            }
            if last_draw.elapsed() > Duration::from_millis(40) {
                last_draw = Instant::now();
                terminal.draw(|frame| {
                    ui(
                        frame,
                        self.focus,
                        self.state.clone(),
                        &mut self.chat_history,
                        &self.input,
                    )
                })?;
            }
            if !event::poll(Duration::from_millis(40))? {
                continue;
            }
            if input_delay.len() == 3 {
                input_delay.pop_front();
            }
            input_delay.push_back(last_input.elapsed());
            last_input = Instant::now();
            match state {
                AppState::Exit => {
                    return Ok(());
                }
                AppState::ReviewDiff(_) => match event::read()? {
                    Event::Key(key) if key.kind == event::KeyEventKind::Press => match key.code {
                        KeyCode::Char('y') => {
                            commit(&self.repo_path)?;
                            // TODO: run in background?
                            // self.client
                            //     .post(&format!(
                            //         "/projects/{}/features/{}/chat/accepted",
                            //         self.project.id, self.feature.id
                            //     ))
                            //     .json(api::GenerationAcceptedRequest {
                            //         message_id: message_id,
                            //         accepted: true,
                            //     })
                            //     .send()
                            //     .await?
                            //     .error_body_for_status()
                            //     .await?;
                            let mut state = self.state.lock().unwrap();
                            *state = AppState::Chat;
                        }
                        KeyCode::Char('n') | KeyCode::Esc => {
                            revert(&self.repo_path)?;
                            // self.client
                            //     .post(&format!(
                            //         "/projects/{}/features/{}/chat/accepted",
                            //         self.project.id, self.feature.id
                            //     ))
                            //     .json(api::GenerationAcceptedRequest {
                            //         message_id: message_id,
                            //         accepted: false,
                            //     })
                            //     .send()
                            //     .await?
                            //     .error_body_for_status()
                            //     .await?;
                            let mut state = self.state.lock().unwrap();
                            *state = AppState::Chat;
                        }
                        KeyCode::Char(' ') => {
                            let mut state = self.state.lock().unwrap();
                            if let AppState::ReviewDiff(diff_widget) = &mut *state {
                                diff_widget.scroll_position = diff_widget
                                    .scroll_position
                                    .saturating_add(10)
                                    .clamp(0, diff_widget.scroll_max);
                                diff_widget.scroll_state = diff_widget
                                    .scroll_state
                                    .position(diff_widget.scroll_position);
                            }
                        }
                        KeyCode::Down => {
                            let mut state = self.state.lock().unwrap();
                            if let AppState::ReviewDiff(diff_widget) = &mut *state {
                                diff_widget.scroll_position = diff_widget
                                    .scroll_position
                                    .saturating_add(1)
                                    .clamp(0, diff_widget.scroll_max);
                                diff_widget.scroll_state = diff_widget
                                    .scroll_state
                                    .position(diff_widget.scroll_position);
                            }
                        }
                        KeyCode::Up => {
                            let mut state = self.state.lock().unwrap();
                            if let AppState::ReviewDiff(diff_widget) = &mut *state {
                                diff_widget.scroll_position = diff_widget
                                    .scroll_position
                                    .saturating_sub(1)
                                    .clamp(0, diff_widget.scroll_max);
                                diff_widget.scroll_state = diff_widget
                                    .scroll_state
                                    .position(diff_widget.scroll_position);
                            }
                        }
                        _ => {}
                    },
                    Event::Mouse(mouse) => match mouse.kind {
                        event::MouseEventKind::ScrollUp => {
                            // TODO: if cursor row within message field, self.input.scroll instead
                            let mut state = self.state.lock().unwrap();
                            if let AppState::ReviewDiff(diff_widget) = &mut *state {
                                diff_widget.scroll_position = diff_widget
                                    .scroll_position
                                    .saturating_sub(1)
                                    .clamp(0, diff_widget.scroll_max);
                                diff_widget.scroll_state = diff_widget
                                    .scroll_state
                                    .position(diff_widget.scroll_position);
                            }
                        }
                        event::MouseEventKind::ScrollDown => {
                            let mut state = self.state.lock().unwrap();
                            if let AppState::ReviewDiff(diff_widget) = &mut *state {
                                diff_widget.scroll_position = diff_widget
                                    .scroll_position
                                    .saturating_add(1)
                                    .clamp(0, diff_widget.scroll_max);
                                diff_widget.scroll_state = diff_widget
                                    .scroll_state
                                    .position(diff_widget.scroll_position);
                            }
                        }
                        _ => {}
                    },
                    _ => {}
                },
                AppState::Help => {
                    if let Event::Key(_) = event::read()? {
                        let mut state = self.state.lock().unwrap();
                        *state = AppState::Chat;
                    }
                }
                AppState::Chat => match event::read()? {
                    Event::FocusGained => {
                        self.focus = true;
                    }
                    Event::FocusLost => {
                        self.focus = false;
                    }
                    Event::Mouse(mouse) => match mouse.kind {
                        event::MouseEventKind::ScrollUp => {
                            self.chat_history.scroll_position =
                                self.chat_history.scroll_position.saturating_sub(1);
                            self.chat_history.scroll_state = self
                                .chat_history
                                .scroll_state
                                .position(self.chat_history.scroll_position);
                        }
                        event::MouseEventKind::ScrollDown => {
                            self.chat_history.scroll_position = self
                                .chat_history
                                .scroll_position
                                .saturating_add(1)
                                .clamp(0, self.chat_history.scroll_max);
                            self.chat_history.scroll_state = self
                                .chat_history
                                .scroll_state
                                .position(self.chat_history.scroll_position);
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
                            let code_blocks = messages.iter_mut().flat_map(|msg| {
                                msg.blocks.iter_mut().filter_map(|block| match block {
                                    MessageBlock::Code(code) => Some(code),
                                    _ => None,
                                })
                            });
                            for ((start, end), block) in self
                                .chat_history
                                .code_block_hitboxes
                                .iter()
                                .zip(code_blocks)
                            {
                                // -1 for the border of chat history
                                if (*start as isize - self.chat_history.scroll_position as isize)
                                    <= (mouse.row as isize) - 1
                                    && (*end as isize - self.chat_history.scroll_position as isize)
                                        > (mouse.row as isize) - 1
                                {
                                    block.folded = !block.folded;
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
                            dbg!("copy");
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
                                let last_generation_done = {
                                    let scrollback = self.chat_history.messages.lock().unwrap();
                                    if let Some(last_msg) = scrollback.last() {
                                        last_msg.finalized
                                    } else {
                                        true
                                    }
                                };
                                if last_generation_done {
                                    self.handle_chat_input(&write).await?;
                                }
                            }
                        }
                        _ => {
                            // TODO: shift-enter for newlines?
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
            match input.as_str() {
                "/exit" | "/quit" => {
                    let mut state = self.state.lock().unwrap();
                    *state = AppState::Exit;
                }
                "/help" => {
                    let mut state = self.state.lock().unwrap();
                    *state = AppState::Help;
                }
                "/docs" => {
                    open::that_detached("https://app.bismuth.cloud/docs")?;
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
                    let mut scrollback = self.chat_history.messages.lock().unwrap();
                    scrollback.push(ChatMessage::new(
                        ChatMessageUser::AI,
                        "I'm sorry, I don't understand that command.",
                    ));
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
    repo_path: &Path,
    client: &APIClient,
) -> Result<()> {
    let repo_path = repo_path.to_path_buf();

    let scrollback: Vec<ChatMessage> = client
        .get(&format!(
            "/projects/{}/features/{}/chat/list",
            project.id, feature.id
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
        current_user,
        &scrollback,
        ws_stream,
        client,
    );

    let status = app.run().await;
    terminal::restore();

    status
}

fn ui(
    frame: &mut ratatui::Frame,
    focus: bool,
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

    /*
    // Make background black when not focused
    // This feels like a bit too much though
    let background = Block::default().bg(if focus {
        ratatui::style::Color::Reset
    } else {
        ratatui::style::Color::Black
    });
    frame.render_widget(background, frame.size());
    */

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
        AppState::Help => {
            let help_text = r#"/exit, /quit, or Esc: Exit the chat
/docs: Open the Bismuth documentation
/help: Show this help"#;
            let paragraph = Paragraph::new(help_text).block(Block::bordered().title(vec![
                "Help ".into(),
                Span::styled("(press any key to close)", ratatui::style::Color::Yellow),
            ]));
            let area = centered_paragraph(&paragraph, frame.area());
            frame.render_widget(Clear, area);
            frame.render_widget(paragraph, area);
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
                DisableFocusChange, DisableMouseCapture, EnableFocusChange, EnableMouseCapture,
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
            EnableFocusChange,
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
            DisableFocusChange,
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
