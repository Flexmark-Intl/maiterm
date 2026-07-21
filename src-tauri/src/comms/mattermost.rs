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

    async fn send<T: serde::de::DeserializeOwned>(
        &self,
        req: reqwest::RequestBuilder,
    ) -> Result<T, CommsError> {
        let resp = req
            .header("Authorization", format!("Bearer {}", self.token))
            .send()
            .await
            .map_err(|e| CommsError::Network(e.to_string()))?;
        let status = resp.status();
        if status.is_success() {
            return resp
                .json::<T>()
                .await
                .map_err(|e| CommsError::Network(format!("bad response body: {e}")));
        }
        let body = resp.text().await.unwrap_or_default();
        Err(match status.as_u16() {
            401 => CommsError::AuthFailed,
            403 => CommsError::Forbidden,
            404 => CommsError::NotFound,
            code => CommsError::Http(code, body.chars().take(300).collect()),
        })
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
    ) -> Result<Post, CommsError> {
        self.send(
            self.http
                .post(format!("{}/api/v4/posts", self.base))
                .json(&serde_json::json!({
                    "channel_id": channel_id,
                    "root_id": root_id,
                    "message": message,
                })),
        )
        .await
    }

    /// The bot's own user record — its id filters the bot's posts out of watcher forwarding.
    pub async fn me(&self) -> Result<User, CommsError> {
        self.send(self.http.get(format!("{}/api/v4/users/me", self.base)))
            .await
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
