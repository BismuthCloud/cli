use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{anyhow, Result};
use futures::{stream::SplitSink, SinkExt, StreamExt, TryStreamExt};
use git2::DiffOptions;
use log::{debug, trace};
use ratatui::{
    crossterm::{
        cursor::SetCursorStyle,
        event::{self, Event, KeyCode, MouseButton},
    },
    layout::{Constraint, Layout, Rect},
    style::Stylize,
    text::{Line, Span},
    widgets::{Block, Clear, Padding, Paragraph, Scrollbar, StatefulWidget, Widget},
};
use syntect::easy::HighlightLines;
use syntect::highlighting::ThemeSet;
use syntect::parsing::SyntaxSet;
use syntect::util::LinesWithEndings;
use syntect_tui::into_span;
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};

use crate::{api, APIClient, ResponseErrorExt as _};

/// Extract files denoted with the BISMUTH FILE comment from a code block.
fn extract_bismuth_files_from_code_block(data: &str) -> HashMap<String, String> {
    let file_regex = lazy_regex::regex!(r"^\s*#\s*BISMUTH FILE\s*:\s*(.*)$");

    let lines: Vec<&str> = data.split('\n').collect();
    let mut current_file_name: Option<String> = None;
    let mut current_file_content = String::new();
    let mut files: HashMap<String, String> = HashMap::new();

    for (lidx, line) in lines.iter().enumerate() {
        if let Some(captures) = file_regex.captures(line) {
            let file_name = captures[1].trim();

            if file_name.ends_with(".md") {
                files.insert(format!("src/{}", file_name), lines[lidx + 1..].join("\n"));
                return files;
            }

            if let Some(current_name) = current_file_name {
                files.insert(current_name, current_file_content.trim().to_string());
                current_file_content.clear();
            }
            current_file_name = Some(file_name.to_string());
        }
        current_file_content.push_str(line);
        current_file_content.push('\n');
    }

    if let Some(current_name) = current_file_name {
        files.insert(current_name, current_file_content.trim().to_string());
    }

    files
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
    let diff = repo.diff_tree_to_workdir(
        Some(&upstream_tree),
        Some(DiffOptions::new().context_lines(3)),
    )?;
    let mut changed_files = vec![];
    diff.foreach(
        &mut |delta, _| {
            changed_files.push(delta.new_file().path().unwrap().to_path_buf());
            true
        },
        None,
        None,
        None,
    )?;
    Ok(changed_files)
}

fn process_chat_message(repo_path: &Path, message: &str) -> Result<Option<String>> {
    let repo_path = std::fs::canonicalize(repo_path)?;
    let repo = git2::Repository::open(&repo_path)?;

    let root = markdown::to_mdast(message, &markdown::ParseOptions::default()).unwrap();
    let code_blocks: Vec<_> = match root.children() {
        Some(nodes) => nodes
            .into_iter()
            .filter_map(|block| match block {
                markdown::mdast::Node::Code(code) => Some(code),
                _ => None,
            })
            .collect(),
        None => return Ok(None),
    };

    if code_blocks.len() == 0 {
        return Ok(None);
    }

    let mut positions = vec![];

    // TODO: do we return a diff for each code block?
    for md_code_block in &code_blocks {
        // This will often only be 1 file, but sometimes the model will output multiple files
        // in one code block.
        let files = extract_bismuth_files_from_code_block(&md_code_block.value);
        for (file_name, content) in files {
            trace!("Writing file: {}", file_name);
            let mut file_name = file_name.as_str();
            file_name = file_name.trim_start_matches('/');
            let full_path = repo_path.join(file_name);
            if !full_path.starts_with(&repo_path) {
                return Err(anyhow::anyhow!("Invalid file path"));
            }
            std::fs::create_dir_all(full_path.parent().unwrap())?;
            std::fs::write(full_path, content)?;
        }
        positions.push(md_code_block.position.clone().unwrap());
    }

    let mut index = repo.index()?;
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
    let repo = git2::Repository::open(&repo_path)?;
    let mut index = repo.index()?;
    let tree_id = index.write_tree()?;
    let tree = repo.find_tree(tree_id)?;

    let head = repo.head()?;
    let parent_commit = repo.find_commit(head.target().unwrap())?;

    let signature = git2::Signature::now("Bismuth", "committer@app.bismuth.cloud")?;

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
    raw_code: String,
    /// The syntax highlighted code
    // TODO: threading <'a> through the whole struct is a complete PITA
    // so just leak stuff for now. It's not really wrong either since these
    // are actually live for the duration of the program.
    lines: Vec<Line<'static>>,
    folded: bool,
}

impl CodeBlock {
    fn new(language: Option<&str>, raw_code: &str) -> Self {
        let raw_code = Box::leak(raw_code.to_string().into_boxed_str());

        let ps = SyntaxSet::load_defaults_newlines();
        let ts = ThemeSet::load_defaults();
        let theme = ts.themes["base16-ocean.dark"].clone();
        let syntax = ps
            .find_syntax_by_extension(match language {
                Some("python") => "py",
                Some("markdown") => "md",
                _ => "txt",
            })
            .unwrap();
        let mut h = HighlightLines::new(syntax, &theme);
        let lines = LinesWithEndings::from(raw_code)
            .map(|line| {
                Line::from(
                    h.highlight_line(line, &ps)
                        .unwrap()
                        .into_iter()
                        .filter_map(|segment| {
                            into_span(segment)
                                .ok()
                                .map(|span| span.bg(ratatui::style::Color::Reset))
                        })
                        .collect::<Vec<Span>>(),
                )
            })
            .collect();

        Self {
            language: language.unwrap_or("").to_string(),
            raw_code: raw_code.to_string(),
            lines,
            folded: true,
        }
    }
}

#[derive(Clone, Debug)]
enum MessageBlock {
    Text(Vec<Line<'static>>),
    Thinking,
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
    blocks: Vec<MessageBlock>,
    raw_content: String,
}

impl ChatMessage {
    fn new(user: ChatMessageUser, content: &str) -> Self {
        let root = markdown::to_mdast(content, &markdown::ParseOptions::default()).unwrap();
        let mut blocks: Vec<_> = match root.children() {
            Some(nodes) => nodes
                .into_iter()
                .map(|block| match block {
                    markdown::mdast::Node::Code(code) => {
                        MessageBlock::Code(CodeBlock::new(code.lang.as_deref(), &code.value))
                    }
                    _ => {
                        // Slice from content based on position instead of node.to_string()
                        // so that we get things like bullet points, list numbering, etc.
                        let position = block.position().unwrap();
                        MessageBlock::new_text(&content[position.start.offset..position.end.offset])
                    }
                })
                .collect(),
            None => vec![],
        };

        let prefix_spans = vec![
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
        ];

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
            blocks,
            raw_content: content.to_string(),
        }
    }
}

impl From<api::ChatMessage> for ChatMessage {
    fn from(message: api::ChatMessage) -> Self {
        ChatMessage::new(
            if message.is_ai {
                ChatMessageUser::AI
            } else {
                ChatMessageUser::User(message.user.as_ref().unwrap().name.clone())
            },
            &message.content,
        )
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
}

impl Widget for &mut ChatHistoryWidget {
    fn render(self, area: ratatui::layout::Rect, buf: &mut ratatui::buffer::Buffer) {
        let block = ratatui::widgets::Block::new()
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
                            MessageBlock::Thinking => {
                                vec![Line::raw(format!(
                                    "Thinking {}",
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
                            .line_count(area.width - 2);
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

        // -2 to account for the borders
        // +3 so you can scroll past the bottom a bit to see this is really the end
        let nlines = paragraph.line_count(area.width - 2) + 3;
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
            .block(Block::bordered().title("Review Diff (y to commit, n to revert)"))
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
    input: String,
    input_cursor: usize,

    ws_stream: Option<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    >,
    state: Arc<Mutex<AppState>>,
    focus: bool,
}

impl App {
    fn new(
        repo_path: &Path,
        current_user: &api::User,
        chat_history: &[ChatMessage],
        ws_stream: tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    ) -> Self {
        Self {
            repo_path: repo_path.to_path_buf(),
            user: current_user.clone(),
            chat_history: ChatHistoryWidget {
                messages: Arc::new(Mutex::new(chat_history.to_vec())),
                scroll_position: 0,
                scroll_max: 0,
                scroll_state: ratatui::widgets::ScrollbarState::default(),
                code_block_hitboxes: vec![],
            },
            input: String::new(),
            input_cursor: 0,
            ws_stream: Some(ws_stream),
            state: Arc::new(Mutex::new(AppState::Chat)),
            focus: true,
        }
    }

    async fn run(&mut self) -> Result<()> {
        let mut terminal = terminal::init()?;

        let (mut write, mut read) = self.ws_stream.take().unwrap().split();
        let (dead_tx, mut dead_rx) = tokio::sync::oneshot::channel();

        let scrollback = self.chat_history.messages.clone();
        let repo_path = self.repo_path.clone();
        let state = self.state.clone();
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
                                scrollback
                                    .last_mut()
                                    .unwrap()
                                    .raw_content
                                    .push_str(&token.text);
                            }
                            api::ws::ChatMessageBody::FinalizedMessage {
                                generated_text, ..
                            } => {
                                {
                                    let mut scrollback = scrollback.lock().unwrap();
                                    let last = scrollback.last_mut().unwrap();
                                    *last = ChatMessage::new(ChatMessageUser::AI, &generated_text);
                                }

                                if let Some(diff) =
                                    process_chat_message(&repo_path, &generated_text).unwrap()
                                {
                                    let mut state = state.lock().unwrap();
                                    *state = AppState::ReviewDiff(DiffReviewWidget::new(diff));
                                }
                            }
                        }
                    }
                    api::ws::Message::ResponseState(state) => match state {
                        api::ws::ResponseState::Parallel => {
                            let mut scrollback = scrollback.lock().unwrap();
                            let last = scrollback.last_mut().unwrap();
                            last.blocks.push(MessageBlock::Thinking);
                        }
                        api::ws::ResponseState::Failed => {}
                    },
                    _ => {}
                }
            }
            dead_tx.send(()).unwrap();
        });

        loop {
            let state = { self.state.lock().unwrap().clone() };
            if let AppState::Exit = state {
                return Ok(());
            }
            if dead_rx.try_recv().is_ok() {
                return Err(anyhow!("Chat connection closed"));
            }
            terminal.draw(|frame| {
                ui(
                    frame,
                    self.focus,
                    self.state.clone(),
                    &mut self.chat_history,
                    &self.input,
                    self.input_cursor,
                )
            })?;
            if !event::poll(Duration::from_millis(100))? {
                continue;
            }
            match state {
                AppState::Exit => {
                    return Ok(());
                }
                AppState::ReviewDiff(_) => match event::read()? {
                    Event::Key(key) if key.kind == event::KeyEventKind::Press => match key.code {
                        KeyCode::Char('y') => {
                            commit(&self.repo_path)?;
                            let mut state = self.state.lock().unwrap();
                            *state = AppState::Chat;
                        }
                        KeyCode::Char('n') | KeyCode::Esc => {
                            revert(&self.repo_path)?;
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
                        event::MouseEventKind::Up(btn) if btn == MouseButton::Left => {
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
                        }
                        _ => {}
                    },
                    Event::Key(key) if key.kind == event::KeyEventKind::Press => match key.code {
                        KeyCode::Char('d')
                            if key.modifiers.contains(event::KeyModifiers::CONTROL) =>
                        {
                            let mut state = self.state.lock().unwrap();
                            *state = AppState::Exit;
                        }
                        KeyCode::Char('w')
                            if key.modifiers.contains(event::KeyModifiers::CONTROL) =>
                        {
                            if self.input_cursor > 0 {
                                self.input_cursor -= 1;
                                self.input.remove(self.input_cursor);
                                while !self.input.is_empty()
                                    && self.input_cursor > 0
                                    && !&self.input[..self.input_cursor].ends_with(' ')
                                {
                                    self.input_cursor -= 1;
                                    self.input.remove(self.input_cursor);
                                }
                            }
                        }
                        KeyCode::Char('e')
                            if key.modifiers.contains(event::KeyModifiers::CONTROL) =>
                        {
                            self.input_cursor = self.input.len();
                        }
                        KeyCode::Char('a')
                            if key.modifiers.contains(event::KeyModifiers::CONTROL) =>
                        {
                            self.input_cursor = 0;
                        }
                        KeyCode::Char(c) => {
                            self.input.insert(self.input_cursor, c);
                            self.input_cursor += 1;
                        }
                        KeyCode::Backspace => {
                            if self.input_cursor > 0 {
                                self.input.remove(self.input_cursor - 1);
                                self.input_cursor -= 1;
                            }
                        }
                        KeyCode::Left => {
                            self.input_cursor = self.input_cursor.saturating_sub(1);
                        }
                        KeyCode::Right => {
                            self.input_cursor = (self.input_cursor + 1).min(self.input.len());
                        }
                        KeyCode::Esc => {
                            let mut state = self.state.lock().unwrap();
                            *state = AppState::Exit;
                        }
                        KeyCode::Enter => {
                            self.handle_chat_input(&mut write).await?;
                        }
                        _ => (),
                    },
                    _ => (),
                },
            }
        }
    }

    async fn handle_chat_input(
        &mut self,
        write: &mut SplitSink<
            tokio_tungstenite::WebSocketStream<
                tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
            >,
            Message,
        >,
    ) -> Result<()> {
        if self.input.trim().is_empty() {
            return Ok(());
        }
        if self.input.starts_with('/') {
            match self.input.as_str() {
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
            self.input.clear();
            self.input_cursor = 0;
            return Ok(());
        }
        let mut scrollback = self.chat_history.messages.lock().unwrap();
        scrollback.push(ChatMessage::new(
            ChatMessageUser::User(self.user.name.clone()),
            &self.input,
        ));
        scrollback.push(ChatMessage::new(ChatMessageUser::AI, ""));
        let modified_files = list_changed_files(&self.repo_path)?
            .into_iter()
            .map(|path| {
                let content = std::fs::read_to_string(&self.repo_path.join(&path)).unwrap();
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
                    message: self.input.clone(),
                    modified_files,
                    request_type_analysis: false,
                }))?
                .into(),
            ))
            .await?;

        self.input.clear();
        self.input_cursor = 0;

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

    let mut url = client.base_url.clone();
    url.set_password(None).unwrap();
    url.set_scheme(&url.scheme().replace("http", "ws")).unwrap();
    url = url.join("/chat/streaming")?;
    let (mut ws_stream, _) = connect_async(url.as_str())
        .await
        .expect("Failed to connect");

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

    let mut app = App::new(&repo_path, current_user, &scrollback, ws_stream);

    let status = app.run().await;
    terminal::restore();

    status
}

fn ui(
    frame: &mut ratatui::Frame,
    focus: bool,
    state: Arc<Mutex<AppState>>,
    chat_history: &mut ChatHistoryWidget,
    input: &str,
    input_cursor: usize,
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
        ratatui::layout::Constraint::Min(3),
    ]);
    let [history_area, input_area] = vertical.areas(frame.size());

    frame.render_widget(chat_history, history_area);

    let input_widget =
        Paragraph::new(input).block(ratatui::widgets::Block::bordered().title("Message"));
    frame.render_widget(input_widget, input_area);

    frame.set_cursor(input_area.x + input_cursor as u16 + 1, input_area.y + 1);

    let mut state = state.lock().unwrap();
    match &mut *state {
        AppState::ReviewDiff(diff_widget) => {
            frame.render_widget(diff_widget, frame.size());
        }
        AppState::Help => {
            let help_text = r#"/exit, /quit, or Esc: Exit the chat
/docs: Open the Bismuth documentation
/help: Show this help"#;
            let paragraph = Paragraph::new(help_text)
                .block(Block::bordered().title("Help (press any key to close)"));
            let area = centered_paragraph(&paragraph, frame.size());
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
