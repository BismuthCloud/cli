use std::fmt::Display;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SubscriptionType {
    Free,
    Team,
    Ent,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Subscription {
    pub r#type: SubscriptionType,
}

#[derive(Clone, Debug, Deserialize)]
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
    pub organization: Organization,
    pub features: Vec<Feature>,
    pub clone_token: String,
    pub github_repo: Option<String>,
    pub github_app_install: Option<GitHubAppInstall>,
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

pub mod ws {
    use serde::{ser::SerializeStruct, Deserialize, Serialize};

    #[derive(Debug, Serialize, Deserialize)]
    #[serde(rename_all = "SCREAMING_SNAKE_CASE")]
    pub enum MessageType {
        Auth,
        Ping,
        Chat,
        ResponseState,
    }

    #[derive(Debug, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct AuthMessage {
        pub token: String,
        pub feature_id: u64,
    }

    #[derive(Debug, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct ChatModifiedFile {
        pub name: String,
        pub project_path: String,
        pub content: String,
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
            output_modified_files: Vec<ChatModifiedFile>,
            id: u64,
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

    #[derive(Debug)]
    pub enum Message {
        Auth(AuthMessage),
        Ping,
        Chat(ChatMessage),
        ResponseState(ResponseStateMessage),
        Error(String),
    }

    impl Serialize for Message {
        fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
        where
            S: serde::Serializer,
        {
            match *self {
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
