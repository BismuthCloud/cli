use std::{
    cell::RefCell,
    collections::HashMap,
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{anyhow, Result};
use futures::{SinkExt, Stream, StreamExt, TryStreamExt};
use log::{debug, error, trace};
use ratatui::{
    crossterm::event::{self, Event, KeyCode},
    layout::Position,
    widgets::{Scrollbar, StatefulWidget, Widget},
};
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message, WebSocketStream};

use crate::{api, APIClient, ResponseErrorExt as _};

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

async fn process_chat_message(repo_path: PathBuf, message: &str) -> Result<Option<String>> {
    let repo_path = std::fs::canonicalize(repo_path)?;
    let repo = git2::Repository::open(&repo_path)?;

    let blocks = markdown::tokenize(message);
    let code_blocks: Vec<_> = blocks
        .iter()
        .filter_map(|block| match block {
            markdown::Block::CodeBlock(language, content) => Some((language, content)),
            _ => None,
        })
        .collect();

    if code_blocks.len() == 0 {
        return Ok(None);
    }

    for (language, code) in &code_blocks {
        let files = extract_bismuth_files_from_code_block(code);
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

    repo.commit(
        Some("HEAD"),
        &signature,
        &signature,
        "foo",
        &tree,
        &[&parent_commit],
    )?;

    Ok(())
}

struct ChatHistoryWidget {
    messages: Arc<Mutex<Vec<api::ChatMessage>>>,
    scroll_position: usize,
    chat_scroll_state: ratatui::widgets::ScrollbarState,
}

impl Widget for &mut ChatHistoryWidget {
    fn render(self, area: ratatui::layout::Rect, buf: &mut ratatui::buffer::Buffer) {
        let block = ratatui::widgets::Block::new()
            .title("Chat History")
            .borders(ratatui::widgets::Borders::ALL);

        let messages = self.messages.lock().unwrap();
        let lines: Vec<_> = messages
            .iter()
            .flat_map(|message| {
                let mut message_lines = message.content.trim().split('\n');
                let mut render_lines = vec![ratatui::text::Line::default().spans(vec![
                    if message.is_ai {
                        ratatui::text::Span::styled(
                            "Bismuth",
                            ratatui::style::Style::default().fg(ratatui::style::Color::Magenta),
                        )
                    } else {
                        ratatui::text::Span::styled(
                            message.user.as_ref().unwrap().name.clone(),
                            ratatui::style::Style::default().fg(ratatui::style::Color::Cyan),
                        )
                    },
                    ": ".into(),
                    message_lines.next().unwrap().into(),
                ])];
                render_lines.extend(message_lines.map(|line| ratatui::text::Line::from(line)));
                render_lines
            })
            .collect();

        self.chat_scroll_state = self.chat_scroll_state.content_length(lines.len());

        let paragraph = ratatui::widgets::Paragraph::new(ratatui::text::Text::from_iter(lines))
            .block(block)
            .scroll((self.scroll_position as u16, 0))
            .wrap(ratatui::widgets::Wrap { trim: false });

        Widget::render(paragraph, area, buf);
        StatefulWidget::render(
            Scrollbar::new(ratatui::widgets::ScrollbarOrientation::VerticalRight),
            area,
            buf,
            &mut self.chat_scroll_state,
        );
    }
}

struct DiffWidget {
    diff: String,
}

enum AppState {
    Chat,
    ReviewDiff(DiffWidget),
}

struct App {
    repo_path: PathBuf,
    chat_history: ChatHistoryWidget,
    /// Current chatbox input
    input: String,
    ws_stream: Option<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    >,
}

impl App {
    fn new(
        repo_path: &Path,
        chat_history: &[api::ChatMessage],
        ws_stream: tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    ) -> Self {
        Self {
            repo_path: repo_path.to_path_buf(),
            chat_history: ChatHistoryWidget {
                messages: Arc::new(Mutex::new(chat_history.to_vec())),
                scroll_position: 0,
                chat_scroll_state: ratatui::widgets::ScrollbarState::default(),
            },
            input: String::new(),
            ws_stream: Some(ws_stream),
        }
    }

    async fn run(&mut self) -> Result<()> {
        let mut terminal = terminal::init()?;

        let (mut write, mut read) = self.ws_stream.take().unwrap().split();
        let (dead_tx, mut dead_rx) = tokio::sync::oneshot::channel();

        let scrollback = self.chat_history.messages.clone();
        let repo_path = self.repo_path.clone();
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
                                scrollback.last_mut().unwrap().content.push_str(&token.text);
                            }
                            api::ws::ChatMessageBody::FinalizedMessage {
                                generated_text, ..
                            } => {
                                {
                                    let mut scrollback = scrollback.lock().unwrap();
                                    scrollback.last_mut().unwrap().content = generated_text.clone();
                                }

                                let diff = process_chat_message(repo_path.clone(), &generated_text)
                                    .await
                                    .unwrap();
                            }
                        }
                    }
                    api::ws::Message::ResponseState(state) => match state {
                        api::ws::ResponseState::Parallel => {
                            // TODO: thinking...
                        }
                        api::ws::ResponseState::Failed => {}
                    },
                    _ => {}
                }
            }
            dead_tx.send(()).unwrap();
        });

        loop {
            if dead_rx.try_recv().is_ok() {
                return Err(anyhow!("Chat connection closed"));
            }
            terminal.draw(|frame| ui(frame, &mut self.chat_history, &self.input))?;
            if !event::poll(Duration::from_millis(100))? {
                continue;
            }
            match event::read()? {
                Event::Mouse(mouse) => {
                    if mouse.kind == event::MouseEventKind::ScrollUp {
                        self.chat_history.scroll_position =
                            self.chat_history.scroll_position.saturating_sub(1);
                        self.chat_history.chat_scroll_state = self
                            .chat_history
                            .chat_scroll_state
                            .position(self.chat_history.scroll_position);
                    } else if mouse.kind == event::MouseEventKind::ScrollDown {
                        self.chat_history.scroll_position =
                            self.chat_history.scroll_position.saturating_add(1);
                        self.chat_history.chat_scroll_state = self
                            .chat_history
                            .chat_scroll_state
                            .position(self.chat_history.scroll_position);
                    }
                }
                Event::Key(key) => {
                    if key.kind != event::KeyEventKind::Press {
                        continue;
                    }
                    match key.code {
                        KeyCode::Char(c) => {
                            self.input.push(c);
                        }
                        KeyCode::Backspace => {
                            self.input.pop();
                        }
                        KeyCode::Esc => {
                            return Ok(());
                        }
                        KeyCode::Enter => {
                            let mut scrollback = self.chat_history.messages.lock().unwrap();
                            scrollback.push(api::ChatMessage {
                                is_ai: false,
                                // TODO
                                user: Some(api::User {
                                    id: 0,
                                    email: "".to_string(),
                                    username: "".to_string(),
                                    name: "You".to_string(),
                                }),
                                content: self.input.clone(),
                            });
                            scrollback.push(api::ChatMessage {
                                is_ai: true,
                                user: None,
                                content: String::new(),
                            });
                            write
                                .send(Message::Text(
                                    serde_json::to_string(&api::ws::Message::Chat(
                                        api::ws::ChatMessage {
                                            message: self.input.clone(),
                                            modified_files: vec![],
                                            request_type_analysis: false,
                                        },
                                    ))?
                                    .into(),
                                ))
                                .await?;
                            self.input.clear();
                        }
                        _ => (),
                    }
                }
                _ => (),
            }
        }
    }
}

pub async fn start_chat(
    project: &api::Project,
    feature: &api::Feature,
    repo_path: &Path,
    client: &APIClient,
) -> Result<()> {
    let repo_path = repo_path.to_path_buf();

    let scrollback: Vec<api::ChatMessage> = client
        .get(&format!(
            "/projects/{}/features/{}/chat/list",
            project.id, feature.id
        ))
        .send()
        .await?
        .error_body_for_status()
        .await?
        .json()
        .await?;

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

    let mut app = App::new(&repo_path, &scrollback, ws_stream);

    let status = app.run().await;
    terminal::restore();

    status
}

fn ui(frame: &mut ratatui::Frame, chat_history: &mut ChatHistoryWidget, input: &str) {
    let vertical = ratatui::layout::Layout::vertical([
        ratatui::layout::Constraint::Percentage(100),
        ratatui::layout::Constraint::Min(3),
    ]);
    let [history_area, input_area] = vertical.areas(frame.area());

    frame.render_widget(chat_history, history_area);

    let input_widget = ratatui::widgets::Paragraph::new(input)
        .block(ratatui::widgets::Block::bordered().title("Message"));
    frame.render_widget(input_widget, input_area);

    frame.set_cursor_position(Position::new(
        input_area.x + input.len() as u16 + 1,
        input_area.y + 1,
    ));
}

mod terminal {
    use std::io;

    use ratatui::{
        backend::CrosstermBackend,
        crossterm::{
            event::DisableMouseCapture,
            event::EnableMouseCapture,
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
        execute!(io::stdout(), EnterAlternateScreen, EnableMouseCapture)?;
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
        if let Err(err) = execute!(io::stdout(), LeaveAlternateScreen, DisableMouseCapture) {
            eprintln!("error leaving alternate screen: {err}");
        }
    }
}
