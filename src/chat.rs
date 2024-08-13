use std::{collections::HashMap, io::Write as _, path::Path, sync::Arc};

use anyhow::Result;
use futures::{pin_mut, SinkExt, StreamExt};
use log::debug;
use tokio::io::AsyncReadExt as _;
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};

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

async fn process_chat_message(repo: &Path, message: &str) -> Result<()> {
    let repo = std::fs::canonicalize(repo)?;
    let blocks = markdown::tokenize(message);
    for (language, code) in blocks.iter().filter_map(|block| match block {
        markdown::Block::CodeBlock(language, content) => Some((language, content)),
        _ => None,
    }) {
        let files = extract_bismuth_files_from_code_block(code);
        for (file_name, content) in files {
            let mut file_name = file_name.as_str();
            file_name = file_name.trim_start_matches('/');
            let full_path = repo.join(file_name);
            if !full_path.starts_with(&repo) {
                return Err(anyhow::anyhow!("Invalid file path"));
            }
            std::fs::create_dir_all(full_path.parent().unwrap())?;
            std::fs::write(full_path, content)?;
        }
    }
    Ok(())
}

pub async fn start_chat(
    project: &api::Project,
    feature: &api::Feature,
    repo: &Path,
    client: &APIClient,
) -> Result<()> {
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

    for message in scrollback {
        println!(
            "{}: {}",
            if message.is_ai {
                "Bismuth"
            } else {
                message.user.as_ref().unwrap().name.as_str()
            },
            message.content
        );
    }

    print!("> ");
    std::io::stdout().flush()?;

    let (stdin_tx, stdin_rx) = futures_channel::mpsc::unbounded();
    tokio::spawn(read_stdin(stdin_tx));

    let (write, read) = ws_stream.split();

    let stdin_to_ws = stdin_rx.map(Ok).forward(write);
    let ws_to_stdout = {
        let buffer = Arc::new(std::sync::Mutex::new(String::new()));
        read.for_each(move |message| {
            let buffer = buffer.clone();
            async move {
                let mut buffer = buffer.lock().unwrap();
                let data: api::ws::Message =
                    serde_json::from_str(&message.unwrap().into_text().unwrap()).unwrap();
                match data {
                    api::ws::Message::Chat(api::ws::ChatMessage { message, .. }) => {
                        let stuff: api::ws::ChatMessageBody =
                            serde_json::from_str(&message).unwrap();
                        match stuff {
                            api::ws::ChatMessageBody::StreamingToken { token, .. } => {
                                buffer.push_str(&token.text);
                                print!("{}", token.text);
                                std::io::stdout().flush().unwrap();
                            }
                            api::ws::ChatMessageBody::FinalizedMessage {
                                generated_text, ..
                            } => {
                                // TODO: clear the whole thing
                                println!("\x1b[2K\r{}", generated_text);
                                process_chat_message(repo, &generated_text).await.unwrap();
                                buffer.clear();
                                print!("> ");
                                std::io::stdout().flush().unwrap();
                            }
                        }
                    }
                    api::ws::Message::ResponseState(state) => match state {
                        api::ws::ResponseState::Parallel => {
                            print!("\n");
                            println!("Thinking...");
                            std::io::stdout().flush().unwrap();
                        }
                        api::ws::ResponseState::Failed => {}
                    },
                    _ => {}
                }
            }
        })
    };

    pin_mut!(stdin_to_ws, ws_to_stdout);
    tokio::select! {
        _ = stdin_to_ws => {}
        _ = ws_to_stdout => {}
    }

    Ok(())
}

async fn read_stdin(tx: futures_channel::mpsc::UnboundedSender<Message>) -> Result<()> {
    let mut stdin = tokio::io::stdin();
    loop {
        let mut buf = vec![0; 1024];
        let n = match stdin.read(&mut buf).await {
            Err(_) | Ok(0) => break,
            Ok(n) => n,
        };
        buf.truncate(n);
        tx.unbounded_send(Message::Text(
            serde_json::to_string(&api::ws::Message::Chat(api::ws::ChatMessage {
                message: String::from_utf8(buf).unwrap(),
                modified_files: vec![],
                request_type_analysis: false,
            }))?
            .into(),
        ))?;
    }
    Ok(())
}
