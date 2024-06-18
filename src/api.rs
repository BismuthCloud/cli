use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
pub struct Organization {
    pub id: u64,
    pub name: String,
}

#[derive(Debug, Deserialize)]
pub struct Feature {
    pub id: u64,
    pub name: String,
}

#[derive(Debug, Deserialize)]
pub struct Project {
    pub id: u64,
    pub name: String,
    pub hash: String,
    pub organization: Organization,
    pub features: Vec<Feature>,
    #[serde(rename = "cloneToken")]
    pub clone_token: String,
}

#[derive(Debug, Serialize)]
pub struct CreateProjectRequest {
    pub name: String,
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
