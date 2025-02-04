use std::fmt::Display;

use log::trace;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SubscriptionType {
    Free, // deprecated
    Individual,
    Professional,
    Team,
    Ent,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Subscription {
    pub id: u64,
    pub r#type: SubscriptionType,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Organization {
    pub id: u64,
    pub name: String,
    pub subscription: Subscription,
}

impl Display for Organization {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.name)
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct Feature {
    pub id: u64,
    pub name: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub id: u64,
    pub name: String,
    pub hash: String,
    pub features: Vec<Feature>,
    pub clone_token: String,
    pub github_repo: Option<String>,
    pub github_app_install: Option<GitHubAppInstall>,
    pub has_pushed: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CreateProjectRepo {
    pub name: String,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum CreateProjectRequest {
    Name(CreateProjectRepo),
    Repo(GitHubRepo),
}

#[derive(Debug, Deserialize)]
pub struct ListProjectsResponse {
    pub projects: Vec<Project>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct FeatureConfig {
    pub key: String,
    pub value: String,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub enum ContainerState {
    Invalid = 0,
    Starting = 1,
    Running = 2,
    Paused = 3,
    Failed = 4,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DeployStatusResponse {
    pub status: ContainerState,
    pub commit: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct InvokeURLResponse {
    pub url: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct User {
    pub id: u64,
    pub email: String,
    pub username: String,
    pub name: String,
pub organizations: Vec<Organization>,
}

///////////////////////////////
// Begin apply_file_edits RPC endpoint and helpers

#[derive(Debug, Serialize, Deserialize)]
pub struct FileEdit {
    pub path: String,
    pub part: String,
    pub replace: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ApplyFileEditsRequest {
    pub edits: Vec<FileEdit>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct FileEditResult {
    pub path: String,
    pub changed: bool,
    pub message: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ApplyFileEditsResponse {
    pub results: Vec<FileEditResult>,
}

fn levenshtein(a: &str, b: &str) -> usize {
    let a_len = a.chars().count();
    let b_len = b.chars().count();
    if a_len == 0 { return b_len; }
    if b_len == 0 { return a_len; }
    let mut prev_row: Vec<usize> = (0..=b_len).collect();
    let mut curr_row = vec![0; b_len + 1];
    for (i, ca) in a.chars().enumerate() {
        curr_row[0] = i + 1;
        for (j, cb) in b.chars().enumerate() {
            let cost = if ca == cb { 0 } else { 1 };
            curr_row[j + 1] = std::cmp::min(
                std::cmp::min(curr_row[j] + 1, prev_row[j + 1] + 1),
                prev_row[j] + cost
            );
        }
        prev_row.clone_from_slice(&curr_row);
    }
    prev_row[b_len]
}

fn similarity_ratio(a: &str, b: &str) -> f64 {
    let lev = levenshtein(a, b) as f64;
    let max_len = a.chars().count().max(b.chars().count()) as f64;
    if max_len == 0.0 { return 1.0; }
    1.0 - lev / max_len
}

pub fn replace_closest_edit_distance(whole: &str, part: &str, replace: &str) -> Option<String> {
    let whole_lines: Vec<&str> = whole.split('\n').collect();
    let part_lines: Vec<&str> = part.split('\n').collect();
    let replace_lines: Vec<&str> = replace.split('\n').collect();

    let scale = 0.1;
    let part_len = part_lines.len();
    let min_len = ((part_len as f64) * (1.0 - scale)).floor() as usize;
    let max_len = ((part_len as f64) * (1.0 + scale)).ceil() as usize;
    let mut best_similarity = 0.0;
    let mut best_start = 0;
    let mut best_end = 0;
    let target = part_lines.join("");

    for length in min_len..=max_len {
        if length == 0 { continue; }
        for i in 0..=whole_lines.len().saturating_sub(length) {
            let end = i + length;
            let chunk = whole_lines[i..end].join("");
            let sim = similarity_ratio(&chunk, &target);
            if sim > best_similarity {
                best_similarity = sim;
                best_start = i;
                best_end = end;
            }
        }
    }

    if best_similarity < 0.8 {
        return None;
    }

    let mut modified_lines = Vec::new();
    modified_lines.extend_from_slice(&whole_lines[..best_start]);
    modified_lines.extend_from_slice(&replace_lines);
    modified_lines.extend_from_slice(&whole_lines[best_end..]);
    Some(modified_lines.join("\n"))
}

fn should_run_commands() -> bool {
    // Toggle for running shell commands based on the RUN_COMMANDS environment variable.
    std::env::var("RUN_COMMANDS").map(|v| v == "true").unwrap_or(true)
}

pub fn handle_apply_file_edits(payload: &str) -> Result<String, String> {
    let req: ApplyFileEditsRequest = serde_json::from_str(payload)
        .map_err(|e| format!("Invalid JSON payload: {}", e))?;
    let mut results = Vec::new();
    let mut any_file_changed = false;

    for edit in req.edits.iter() {
        match std::fs::read_to_string(&edit.path) {
            Ok(content) => {
                match replace_closest_edit_distance(&content, &edit.part, &edit.replace) {
                    Some(new_content) => {
                        if new_content != content {
                            if let Err(e) = std::fs::write(&edit.path, new_content) {
                                results.push(FileEditResult {
                                    path: edit.path.clone(),
                                    changed: false,
                                    message: Some(format!("Failed to write file: {}", e)),
                                });
                                continue;
                            }
                            any_file_changed = true;
                            results.push(FileEditResult {
                                path: edit.path.clone(),
                                changed: true,
                                message: Some("File modified".to_string()),
                            });
                        } else {
                            results.push(FileEditResult {
                                path: edit.path.clone(),
                                changed: false,
                                message: Some("No changes applied".to_string()),
                            });
                        }
                    },
                    None => {
                        results.push(FileEditResult {
                            path: edit.path.clone(),
                            changed: false,
                            message: Some("No suitable match found".to_string()),
                        });
                    }
                }
            },
            Err(e) => {
                results.push(FileEditResult {
                    path: edit.path.clone(),
                    changed: false,
                    message: Some(format!("Failed to read file: {}", e)),
                });
            }
        }
    }

    if any_file_changed && should_run_commands() {
        let commit_status = std::process::Command::new("git")
            .args(&["commit", "-am", "Applied file edits"])
            .status();
        match commit_status {
            Ok(status) if status.success() => {},
            Ok(status) => return Err(format!("Git commit failed with exit code: {}", status)),
            Err(e) => return Err(format!("Failed to run git commit: {}", e)),
        }
    }

    serde_json::to_string(&ApplyFileEditsResponse { results })
        .map_err(|e| format!("Failed to serialize response: {}", e))
}

// End apply_file_edits RPC endpoint and helpers

}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GitHubRepo {
    pub installation_id: u64,
    pub repo: String,
}

impl Display for GitHubRepo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.repo)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitHubAppInstall {
    pub installation_id: u64,
    pub organization: Option<String>,
}

impl Display for GitHubAppInstall {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.organization.as_ref().unwrap())
    }
}

fn default_mode() -> String {
    "multi".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SessionMode {
    MultiTurn,
    SingleTurn,
}

#[derive(Clone, Debug, Serialize)]
pub struct ContextStorage {
    #[serde(default)]
    pub pinned_files: Vec<String>,
    #[serde(default = "default_mode")]
    pub mode: String,
    #[serde(default = "default_model")]
    pub model: String,
}

impl<'de> Deserialize<'de> for ContextStorage {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Helper {
            #[serde(default = "default_mode")]
            mode: String,
            #[serde(default)]
            pinned_files: Vec<String>,
            #[serde(default = "default_model")]
            model: String,
        }

        let value = serde_json::Value::deserialize(deserializer)?;

        match value {
            serde_json::Value::String(s) => {
                trace!("Custom deserialize string: {:?}", s);

                if s == "null" {
                    Ok(ContextStorage {
                        mode: "multi".to_string(),
                        pinned_files: vec![],
                        model: "auto".to_string(),
                    })
                } else {
                    let helper: Helper =
                        serde_json::from_str(&s).map_err(serde::de::Error::custom)?;
                    Ok(ContextStorage {
                        mode: helper.mode,
                        pinned_files: helper.pinned_files,
                        model: helper.model,
                    })
                }
            }
            value => {
                trace!("Custom deserialize: {:?}", value);
                let helper: Helper =
                    serde_json::from_value(value).map_err(serde::de::Error::custom)?;
                Ok(ContextStorage {
                    mode: helper.mode,
                    pinned_files: helper.pinned_files,
                    model: helper.model,
                })
            }
        }
    }


pub fn default_model() -> String {
    "auto".to_string()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChatSession {
    pub id: u64,
    #[serde(rename = "name")]
    pub _name: Option<String>,
    #[serde(rename = "context_storage")]
    pub _context_storage: Option<ContextStorage>,
}

impl ChatSession {
    pub fn name(&self) -> String {
        match &self._name {
            Some(name) => name.clone(),
            None => format!("session-{}", self.id),
        }
    }

    pub fn pinned_files(&self) -> Vec<String> {
        match self._context_storage.clone() {
            Some(storage) => storage.pinned_files,
            _ => vec![],
        }
    }

    pub fn swap_model(&mut self, model: String) {
        match self._context_storage.clone() {
            Some(mut storage) => {
                storage.model = model;
                self._context_storage = Some(storage);
            }
            _ => {
                self._context_storage = Some(ContextStorage {
                    pinned_files: vec![],
                    mode: "multi".to_string(),
                    model: model,
                });
            }
        }
    }

    pub fn swap_mode(&mut self) {
        match self._context_storage.clone() {
            Some(mut storage) => {
                storage.mode = match storage.mode.as_str() {
                    "single" => "chat",
                    "multi" => "single",
                    "chat" => "multi",
                    _ => "single",
                }
                .to_string();

                self._context_storage = Some(storage);
            }
            _ => {
                self._context_storage = Some(ContextStorage {
                    pinned_files: vec![],
                    mode: "single".to_string(),
                    model: "auto".to_string(),
                });
            }
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ChatMessage {
    #[serde(rename = "isAI")]
    pub is_ai: bool,
    pub user: Option<User>,
    pub content: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerationAcceptedRequest {
    pub message_id: u64,
    pub accepted: bool,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LLMConfigurationRequest {
    pub key: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreditUsage {
    pub plan_included: i32,
    pub plan_used: i32,
    pub purchased_remaining: i32,
}

pub mod ws {
    use serde::{ser::SerializeStruct, Deserialize, Serialize};

    }
    #[derive(Debug, Serialize, Deserialize, Clone)]
    #[serde(rename_all = "camelCase")]
    pub struct WebSocketEditMessage {
        pub edits: Vec<FileEdit>,
    }
    #[derive(Debug, Serialize, Deserialize)]
    #[serde(rename_all = "SCREAMING_SNAKE_CASE")]
    pub enum MessageType {
        Auth,
        Ping,
        Chat,
        ResponseState,
        Model,
        ModelList,
    }

    #[derive(Debug, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct AuthMessage {
        pub token: String,
        pub session_id: u64,
        pub feature_id: u64,
    }

    #[derive(Clone, Debug, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct ChatModifiedFile {
        pub name: String,
        pub project_path: String,
        pub content: String,
        pub deleted: Option<bool>,
    }

    #[derive(Debug, Serialize, Deserialize)]
    pub struct StreamingChatMessageToken {
        pub text: String,
    }

    #[derive(Debug, Serialize, Deserialize)]
    #[serde(untagged)]
    pub enum ChatMessageBody {
        StreamingToken {
            token: StreamingChatMessageToken,
            clear_past_output: bool,
            id_at_analysis_open: Option<u64>,
        },
        // The entirety of a message, but one that is still generating
        // Used to send over prologue + code blocks for formatting in one go after parallel generation.
        PartialMessage {
            partial_message: String,
        },
        FinalizedMessage {
            done: bool,
            generated_text: String,
            commit_message: Option<String>,
            output_modified_files: Vec<ChatModifiedFile>,
            id: u64,
            // Option only for transitional
            credits_used: Option<u64>,
        },
    }

    #[derive(Debug, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct ChatMessage {
        pub message: String,

        // Serialize only fields
        #[serde(default)]
        pub modified_files: Vec<ChatModifiedFile>,
        #[serde(default)]
        pub request_type_analysis: bool,
    }

    #[derive(Debug, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct ResponseStateMessage {
        pub state: String,
        pub attempt: u64,
    }

    #[derive(Clone, Debug, Deserialize)]
    pub struct RunCommandMessage {
        pub output_modified_files: Vec<ChatModifiedFile>,
        pub command: String,
    }

    #[derive(Clone, Debug, Deserialize, Serialize)]
    pub struct PinFileMessage {
        pub path: String,
    }

    #[derive(Clone, Debug, Deserialize, Serialize)]
    pub struct SwitchModelResponseMessage {
        pub model: String,
    }

    #[derive(Debug, Serialize)]
    pub struct RunCommandResponse {
        pub exit_code: i32,
        pub output: String,
        pub modified_files: Vec<ChatModifiedFile>,
    }

    #[derive(Debug, Deserialize)]
    #[serde(tag = "action", rename_all = "SCREAMING_SNAKE_CASE")]
    pub enum FileRPCRequest {
        List,
        Read { path: String },
        Search { query: String },
    }

    #[derive(Debug, Serialize)]
    #[serde(tag = "action", rename_all = "SCREAMING_SNAKE_CASE")]
    pub enum FileRPCResponse {
        List {
            files: Vec<String>,
        },
        Read {
            content: Option<String>,
        },
        Search {
            // (filename, line number, line content)
            results: Vec<(String, usize, String)>,
        },
    }

    #[derive(Debug, Deserialize)]
    #[serde(tag = "action", rename_all = "SCREAMING_SNAKE_CASE")]
    pub enum ACIMessage {
        Start {
            files: Vec<String>,
            active_file: String,
            new_contents: String,
            scroll_position: usize,
        },
        Scroll {
            status: String,
            scroll_position: usize,
        },
        Create {
            status: String,
            active_file: String,
            new_contents: String,
            files: Vec<String>,
            scroll_position: usize,
        },
        Switch {
            status: String,
            active_file: String,
            new_contents: String,
            scroll_position: usize,
        },
        Close {
            status: String,
        },
        Edit {
            status: String,
            new_contents: String,
            scroll_position: usize,
            changed_range: (usize, usize),
        },
        Test {
            status: String,
            test_output: String,
        },
        Status {
            status: String,
        },
        End,
    }

    #[derive(Debug)]
    pub enum Message {
        Auth(AuthMessage),
        Ping,
        Chat(ChatMessage),
        ResponseState(ResponseStateMessage),
        RunCommand(RunCommandMessage),
    RunCommandResponse(RunCommandResponse),
    WebSocketEdit(WebSocketEditMessage),
    ACI(ACIMessage),
        FileRPC(FileRPCRequest),
        FileRPCResponse(FileRPCResponse),
        KillGeneration,
        Error(String),
        Usage(u64),
        SwitchMode,
        SwitchModeResponse,
        PinFile(PinFileMessage),
        PinFileResponse,
        SwitchModel(String),
        SwitchModelResponse(SwitchModelResponseMessage),
    }

    impl Serialize for Message {
        fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
        where
            S: serde::Serializer,
        {
            match *self {
                Message::SwitchMode => {
                    let mut state = serializer.serialize_struct("Message", 1)?;
                    state.serialize_field("type", "SWITCH_MODE")?;
                    state.end()
                }
                Message::SwitchModeResponse => {
                    let mut state = serializer.serialize_struct("Message", 1)?;
                    state.serialize_field("type", "SWITCH_MODE_RESPONSE")?;
                    state.end()
                }
                Message::PinFileResponse => {
                    let mut state = serializer.serialize_struct("Message", 1)?;
                    state.serialize_field("type", "PIN_FILE_RESPONSE")?;
                    state.end()
                }
                Message::PinFile(ref pin) => {
                    let mut state = serializer.serialize_struct("Message", 2)?;
                    state.serialize_field("type", "PIN_FILE")?;
                    state.serialize_field("pin", pin)?;
                    state.end()
                }
                Message::Auth(ref auth) => {
                    let mut state = serializer.serialize_struct("Message", 2)?;
                    state.serialize_field("type", "AUTH")?;
                    state.serialize_field("auth", auth)?;
                    state.end()
                }
                Message::Ping => {
                    let mut state = serializer.serialize_struct("Message", 1)?;
                    state.serialize_field("type", "PING")?;
                    state.end()
                }
                Message::Chat(ref chat) => {
                    let mut state = serializer.serialize_struct("Message", 2)?;
                    state.serialize_field("type", "CHAT")?;
                    state.serialize_field("chat", chat)?;
                    state.end()
                }
                Message::RunCommandResponse(ref response) => {
                    let mut state = serializer.serialize_struct("Message", 4)?;
                    state.serialize_field("type", "RUN_COMMAND_RESPONSE")?;
                    state.serialize_field("runCommandResponse", response)?;
                    state.end()
                }
                Message::WebSocketEdit(ref msg) => {
                    let mut state = serializer.serialize_struct("Message", 2)?;
                    state.serialize_field("type", "WEBSOCKET_EDIT")?;
                    state.serialize_field("webSocketEdit", msg)?;
                    state.end()
                }
                Message::KillGeneration => {
                    let mut state = serializer.serialize_struct("Message", 1)?;
                    state.serialize_field("type", "KILL_GENERATION")?;
                    state.end()
                }
                Message::FileRPCResponse(ref response) => {
                    let mut state = serializer.serialize_struct("Message", 3)?;
                    state.serialize_field("type", "FILE_RPC_RESPONSE")?;
                    state.serialize_field("file_rpc_response", response)?;
                    state.end()
                }
                Message::SwitchModel(ref model) => {
                    let mut state = serializer.serialize_struct("Message", 2)?;
                    state.serialize_field("type", "SWITCH_MODEL")?;
                    state.serialize_field("model", model)?;
                    state.end()
                }
                Message::SwitchModelResponse(ref response) => {
                    let mut state = serializer.serialize_struct("Message", 2)?;
                    state.serialize_field("type", "SWITCH_MODEL_RESPONSE")?;
                    state.serialize_field("switchModelResponse", response)?;
                    state.end()
                }
                // ResponseState is one-way, no need to serialize
                _ => unimplemented!(),
            }
        }
    }

    impl<'a> Deserialize<'a> for Message {
        fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
        where
            D: serde::Deserializer<'a>,
        {
            let value = serde_json::Value::deserialize(deserializer)?;
            let message_type = value.get("type").and_then(|v| v.as_str());
            match message_type {
                // AUTH is a one-way message, so we don't need to deserialize it
                Some("PING") => Ok(Message::Ping),
                Some("CHAT") => {
                    let chat = serde_json::from_value(
                        value
                            .get("chat")
                            .ok_or(serde::de::Error::custom("missing inner chat"))?
                            .clone(),
                    )
                    .map_err(serde::de::Error::custom)?;
                    Ok(Message::Chat(chat))
                }
                Some("RESPONSE_STATE") => {
                    let state = serde_json::from_value(
                        value
                            .get("responseState")
                            .ok_or(serde::de::Error::custom("missing inner response state"))?
                            .clone(),
                    )
                    .map_err(serde::de::Error::custom)?;
                    Ok(Message::ResponseState(state))
                }
                Some("SWITCH_MODE_RESPONSE") => Ok(Message::SwitchModeResponse),
                Some("SWITCH_MODEL_RESPONSE") => {
                    let response = serde_json::from_value(
                        value
                            .get("switchModelResponse")
                            .ok_or(serde::de::Error::custom("missing inner response state"))?
                            .clone(),
                    )
                    .map_err(serde::de::Error::custom)?;

                    Ok(Message::SwitchModelResponse(response))
                }
                Some("PIN_FILE_RESPONSE") => Ok(Message::PinFileResponse),
                Some("RUN_COMMAND") => {
                    let command = serde_json::from_value(
                        value.get("run_command")
                            .ok_or(serde::de::Error::custom("missing inner run command"))?
                            .clone(),
                    )
                    .map_err(serde::de::Error::custom)?;
                    Ok(Message::RunCommand(command))
                }
                Some("WEBSOCKET_EDIT") => {
                    let message = serde_json::from_value(
                        value.get("webSocketEdit")
                            .ok_or(serde::de::Error::custom("missing inner webSocketEdit"))?
                            .clone()
                    ).map_err(serde::de::Error::custom)?;
                    Ok(Message::WebSocketEdit(message))
                }
                Some("SWITCH_MODEL") => {
                    let model = serde_json::from_value(
                        value.get("model")
                            .ok_or(serde::de::Error::custom("missing inner model"))?
                            .clone(),
                    )
                    .map_err(serde::de::Error::custom)?;
                    Ok(Message::SwitchModel(model))
                }
                }
                Some("ACI") => {
                    let aci = serde_json::from_value(
                        value
                            .get("aci")
                            .ok_or(serde::de::Error::custom("missing inner aci"))?
                            .clone(),
                    )
                    .map_err(serde::de::Error::custom)?;
                    Ok(Message::ACI(aci))
                }
                Some("FILE_RPC") => {
                    let req = serde_json::from_value(
                        value
                            .get("file_rpc")
                            .ok_or(serde::de::Error::custom("missing inner file_rpc"))?
                            .clone(),
                    )
                    .map_err(serde::de::Error::custom)?;
                    Ok(Message::FileRPC(req))
                }
                Some("USAGE") => {
                    let response = serde_json::from_value(
                        value
                            .get("usage")
                            .ok_or(serde::de::Error::custom("missing inner usage"))?
                            .clone(),
                    )
                    .map_err(serde::de::Error::custom)?;
                    Ok(Message::Usage(response))
                }
                None if value.get("error").is_some() => {
                    // Handle generic {"error": "asdf"} messages that come if the backend raises an error
                    return Ok(Message::Error(
                        value.get("error").unwrap().as_str().unwrap().to_string(),
                    ));
                }
                _ => Err(serde::de::Error::custom("invalid message type")),
            }
        }
    }
}