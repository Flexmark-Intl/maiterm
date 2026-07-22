//! Thin Mattermost REST client (bot-token auth) for the comms integration.
//! Only the handful of v4 endpoints /maiterm resolve needs — see docs at
//! https://api.mattermost.com. All calls run on the machine hosting the Rust
//! backend (SSH-bridged sessions tunnel back here, so egress is always local).

use std::collections::HashMap;

use serde::Deserialize;

use super::CommsError;

#[derive(Debug, Clone, Deserialize)]
pub struct Post {
    pub id: String,
    /// Empty string when the post IS the thread root.
    #[serde(default)]
    pub root_id: String,
    pub channel_id: String,
    pub user_id: String,
    #[serde(default)]
    pub message: String,
    /// Milliseconds since epoch.
    pub create_at: i64,
    /// Attachment file ids. Names/mime types ride in `metadata.files` when the
    /// server includes it (v5.6+ always does for post fetches); `file_info` is
    /// the per-id fallback.
    #[serde(default)]
    pub file_ids: Vec<String>,
    #[serde(default)]
    pub metadata: PostMetadata,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct PostMetadata {
    #[serde(default)]
    pub files: Vec<FileInfo>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FileInfo {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub mime_type: String,
    #[serde(default)]
    pub size: i64,
}

/// Response envelope of POST /api/v4/files.
#[derive(Debug, Deserialize)]
struct FileUploadResponse {
    #[serde(default)]
    file_infos: Vec<FileInfo>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Team {
    pub id: String,
    /// URL name (used in permalinks: https://server/<name>/pl/<post_id>).
    pub name: String,
    #[serde(default)]
    pub display_name: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Channel {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub display_name: String,
    /// "O" open, "P" private, "D" direct, "G" group.
    #[serde(rename = "type", default)]
    pub channel_type: String,
    #[serde(default)]
    pub team_id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct User {
    pub id: String,
    pub username: String,
    #[serde(default)]
    pub first_name: String,
    #[serde(default)]
    pub last_name: String,
    #[serde(default)]
    pub nickname: String,
}

/// Mattermost's thread envelope: `order` is post ids, `posts` the id→post map.
#[derive(Debug, Deserialize)]
struct PostList {
    #[serde(default)]
    order: Vec<String>,
    #[serde(default)]
    posts: HashMap<String, Post>,
}

pub struct MattermostClient {
    base: String,
    token: String,
    http: reqwest::Client,
}

impl MattermostClient {
    pub fn new(base_url: &str, token: &str, http: reqwest::Client) -> Self {
        Self {
            base: base_url.trim().trim_end_matches('/').to_string(),
            token: token.trim().to_string(),
            http,
        }
    }

    pub fn base_url(&self) -> &str {
        &self.base
    }

    /// Fire a request and, on a non-success status, map it to a CommsError.
    async fn send_raw(&self, req: reqwest::RequestBuilder) -> Result<reqwest::Response, CommsError> {
        let resp = req
            .header("Authorization", format!("Bearer {}", self.token))
            .send()
            .await
            .map_err(|e| CommsError::Network(e.to_string()))?;
        let status = resp.status();
        if status.is_success() {
            return Ok(resp);
        }
        let body = resp.text().await.unwrap_or_default();
        Err(match status.as_u16() {
            401 => CommsError::AuthFailed,
            403 => CommsError::Forbidden,
            404 => CommsError::NotFound,
            code => CommsError::Http(code, body.chars().take(300).collect()),
        })
    }

    async fn send<T: serde::de::DeserializeOwned>(
        &self,
        req: reqwest::RequestBuilder,
    ) -> Result<T, CommsError> {
        self.send_raw(req)
            .await?
            .json::<T>()
            .await
            .map_err(|e| CommsError::Network(format!("bad response body: {e}")))
    }

    pub async fn get_post(&self, post_id: &str) -> Result<Post, CommsError> {
        self.send(self.http.get(format!("{}/api/v4/posts/{post_id}", self.base)))
            .await
    }

    /// The whole thread containing `post_id`, sorted by create_at ascending.
    pub async fn get_thread(&self, post_id: &str) -> Result<Vec<Post>, CommsError> {
        let list: PostList = self
            .send(
                self.http
                    .get(format!("{}/api/v4/posts/{post_id}/thread", self.base)),
            )
            .await?;
        let mut posts: Vec<Post> = list.posts.into_values().collect();
        // `order` isn't chronological for threads; create_at is authoritative.
        let _ = list.order;
        posts.sort_by_key(|p| (p.create_at, p.id.clone()));
        Ok(posts)
    }

    pub async fn create_post(
        &self,
        channel_id: &str,
        root_id: &str,
        message: &str,
        file_ids: &[String],
    ) -> Result<Post, CommsError> {
        let mut body = serde_json::json!({
            "channel_id": channel_id,
            "root_id": root_id,
            "message": message,
        });
        if !file_ids.is_empty() {
            body["file_ids"] = serde_json::json!(file_ids);
        }
        self.send(
            self.http
                .post(format!("{}/api/v4/posts", self.base))
                .json(&body),
        )
        .await
    }

    /// Raw bytes of an uploaded file (attachment download).
    pub async fn get_file(&self, file_id: &str) -> Result<Vec<u8>, CommsError> {
        self.send_raw(self.http.get(format!("{}/api/v4/files/{file_id}", self.base)))
            .await?
            .bytes()
            .await
            .map(|b| b.to_vec())
            .map_err(|e| CommsError::Network(e.to_string()))
    }

    /// Name/mime/size for a file id — fallback for posts whose `metadata.files` is absent.
    pub async fn file_info(&self, file_id: &str) -> Result<FileInfo, CommsError> {
        self.send(
            self.http
                .get(format!("{}/api/v4/files/{file_id}/info", self.base)),
        )
        .await
    }

    /// Upload one file into a channel; returns its file id for attaching to a post.
    pub async fn upload_file(
        &self,
        channel_id: &str,
        filename: &str,
        bytes: Vec<u8>,
    ) -> Result<String, CommsError> {
        let part = reqwest::multipart::Part::bytes(bytes).file_name(filename.to_string());
        let form = reqwest::multipart::Form::new()
            .text("channel_id", channel_id.to_string())
            .part("files", part);
        let resp: FileUploadResponse = self
            .send(
                self.http
                    .post(format!("{}/api/v4/files", self.base))
                    .multipart(form),
            )
            .await?;
        resp.file_infos
            .into_iter()
            .next()
            .map(|f| f.id)
            .ok_or_else(|| CommsError::Network("upload succeeded but returned no file info".into()))
    }

    /// The bot's own user record — its id filters the bot's posts out of watcher forwarding.
    pub async fn me(&self) -> Result<User, CommsError> {
        self.send(self.http.get(format!("{}/api/v4/users/me", self.base)))
            .await
    }

    /// Teams the authenticated (bot) user belongs to.
    pub async fn my_teams(&self) -> Result<Vec<Team>, CommsError> {
        self.send(self.http.get(format!("{}/api/v4/users/me/teams", self.base)))
            .await
    }

    /// Channels the authenticated (bot) user is a member of within a team.
    pub async fn my_team_channels(&self, team_id: &str) -> Result<Vec<Channel>, CommsError> {
        self.send(self.http.get(format!(
            "{}/api/v4/users/me/teams/{team_id}/channels",
            self.base
        )))
        .await
    }

    /// Posts in a channel newer than `since_ms`, sorted by create_at ascending.
    pub async fn channel_posts_since(
        &self,
        channel_id: &str,
        since_ms: i64,
    ) -> Result<Vec<Post>, CommsError> {
        let list: PostList = self
            .send(self.http.get(format!(
                "{}/api/v4/channels/{channel_id}/posts?since={since_ms}",
                self.base
            )))
            .await?;
        let mut posts: Vec<Post> = list.posts.into_values().collect();
        posts.sort_by_key(|p| (p.create_at, p.id.clone()));
        Ok(posts)
    }

    pub async fn users_by_ids(&self, ids: &[String]) -> Result<Vec<User>, CommsError> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        self.send(
            self.http
                .post(format!("{}/api/v4/users/ids", self.base))
                .json(&ids),
        )
        .await
    }
}
