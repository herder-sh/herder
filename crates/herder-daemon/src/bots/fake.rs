//! A bridge for tests: records what the bot sends, and serves the files and threads tests give
//! it.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures_util::future::BoxFuture;
use tokio::sync::watch;

use super::bridge::{self, Bridge, ChatFile, Earlier, HistoryError, Place, Post};

/// Something the bot sent.
#[derive(Clone, Debug, PartialEq)]
pub enum Sent {
    Post {
        place: Place,
        id: String,
        post: Post,
    },
    Edit {
        place: Place,
        id: String,
        post: Post,
    },
    Delete {
        place: Place,
        id: String,
    },
    Ephemeral {
        place: Place,
        user_id: String,
        text: String,
    },
    Snippet {
        place: Place,
        title: String,
        text: String,
    },
    Upload {
        place: Place,
        name: String,
        data: Vec<u8>,
    },
}

/// A chat that records everything sent to it.
pub struct FakeBridge {
    sent: watch::Sender<Vec<Sent>>,
    /// Files messages carry, by url.
    files: HashMap<String, Vec<u8>>,
    /// Whether it refuses uploads, as a chat that is down or full does.
    refuses_uploads: AtomicBool,
    /// What reading each thread answers.
    threads: Mutex<HashMap<Place, Result<Vec<Earlier>, HistoryError>>>,
    /// What reading each channel's latest messages answers, by channel.
    channels: Mutex<HashMap<String, Vec<Earlier>>>,
}

impl FakeBridge {
    pub fn new(files: HashMap<String, Vec<u8>>) -> Self {
        Self {
            sent: watch::Sender::new(Vec::new()),
            files,
            refuses_uploads: AtomicBool::new(false),
            threads: Mutex::default(),
            channels: Mutex::default(),
        }
    }

    /// Refuses every upload from now on.
    pub fn refuse_uploads(&self) {
        self.refuses_uploads.store(true, Ordering::Relaxed);
    }

    /// Makes reading the latest messages of `channel_id` answer `messages`, oldest first, each
    /// of which a read gets when its id sorts before `before`.
    pub fn channel(&self, channel_id: &str, messages: Vec<Earlier>) {
        let mut channels = self.channels.lock().unwrap();
        channels.insert(channel_id.to_owned(), messages);
    }

    /// Makes reading the thread `place` answer `history`: its messages, oldest first, each of
    /// which a read gets when its id sorts after `after` and before `before`, as Slack's
    /// timestamps of one length do; or why it cannot be read.
    pub fn thread(&self, place: &Place, history: Result<Vec<Earlier>, HistoryError>) {
        let mut threads = self.threads.lock().unwrap();
        threads.insert(place.clone(), history);
    }

    /// Everything sent so far.
    pub fn sent(&self) -> Vec<Sent> {
        self.sent.borrow().clone()
    }

    /// Waits until what was sent satisfies `done`, for at most ten seconds; returns it.
    pub async fn until(&self, done: impl Fn(&[Sent]) -> bool) -> Vec<Sent> {
        let mut sent = self.sent.subscribe();
        let waited = tokio::time::timeout(Duration::from_secs(10), sent.wait_for(|s| done(s)));
        match waited.await {
            Ok(Ok(sent)) => sent.clone(),
            _ => panic!(
                "the bot never sent what was expected; it sent {:#?}",
                self.sent()
            ),
        }
    }

    fn record(&self, sent: Sent) {
        self.sent.send_modify(|all| all.push(sent));
    }
}

impl Bridge for FakeBridge {
    fn post<'a>(
        &'a self,
        place: &'a Place,
        post: &'a Post,
    ) -> BoxFuture<'a, bridge::Result<String>> {
        let id = format!("m{}", self.sent.borrow().len() + 1);
        self.record(Sent::Post {
            place: place.clone(),
            id: id.clone(),
            post: post.clone(),
        });
        Box::pin(async move { Ok(id) })
    }

    /// The direct message with `U…` is `D…`, as the recorded Slack payloads have it.
    fn direct<'a>(
        &'a self,
        user_id: &'a str,
        post: &'a Post,
    ) -> BoxFuture<'a, bridge::Result<Place>> {
        let id = format!("m{}", self.sent.borrow().len() + 1);
        let place = Place {
            channel_id: format!("D{}", user_id.trim_start_matches('U')),
            thread_id: id.clone(),
        };
        self.record(Sent::Post {
            place: place.clone(),
            id,
            post: post.clone(),
        });
        Box::pin(async move { Ok(place) })
    }

    fn edit<'a>(
        &'a self,
        place: &'a Place,
        message_id: &'a str,
        post: &'a Post,
    ) -> BoxFuture<'a, bridge::Result<()>> {
        self.record(Sent::Edit {
            place: place.clone(),
            id: message_id.into(),
            post: post.clone(),
        });
        Box::pin(async { Ok(()) })
    }

    fn delete<'a>(
        &'a self,
        place: &'a Place,
        message_id: &'a str,
    ) -> BoxFuture<'a, bridge::Result<()>> {
        self.record(Sent::Delete {
            place: place.clone(),
            id: message_id.into(),
        });
        Box::pin(async { Ok(()) })
    }

    fn ephemeral<'a>(
        &'a self,
        place: &'a Place,
        user_id: &'a str,
        text: &'a str,
    ) -> BoxFuture<'a, bridge::Result<()>> {
        self.record(Sent::Ephemeral {
            place: place.clone(),
            user_id: user_id.into(),
            text: text.into(),
        });
        Box::pin(async { Ok(()) })
    }

    fn snippet<'a>(
        &'a self,
        place: &'a Place,
        title: &'a str,
        text: &'a str,
    ) -> BoxFuture<'a, bridge::Result<()>> {
        self.record(Sent::Snippet {
            place: place.clone(),
            title: title.into(),
            text: text.into(),
        });
        Box::pin(async { Ok(()) })
    }

    fn upload<'a>(
        &'a self,
        place: &'a Place,
        name: &'a str,
        data: Vec<u8>,
    ) -> BoxFuture<'a, bridge::Result<()>> {
        if self.refuses_uploads.load(Ordering::Relaxed) {
            return Box::pin(async { Err("the upload was refused".to_owned()) });
        }
        self.record(Sent::Upload {
            place: place.clone(),
            name: name.into(),
            data,
        });
        Box::pin(async { Ok(()) })
    }

    fn download<'a>(&'a self, file: &'a ChatFile) -> BoxFuture<'a, bridge::Result<Vec<u8>>> {
        let data = self.files.get(&file.url).cloned();
        Box::pin(async move { data.ok_or_else(|| "no such file".to_owned()) })
    }

    fn history<'a>(
        &'a self,
        place: &'a Place,
        after: Option<&'a str>,
        before: &'a str,
    ) -> BoxFuture<'a, Result<Vec<Earlier>, HistoryError>> {
        let threads = self.threads.lock().unwrap();
        let read = match threads.get(place).cloned().unwrap_or(Ok(Vec::new())) {
            Ok(messages) => Ok(messages
                .into_iter()
                .filter(|message| {
                    let id = message.message_id.as_str();
                    after.is_none_or(|after| id > after) && id < before
                })
                .collect()),
            Err(err) => Err(err),
        };
        Box::pin(async move { read })
    }

    fn recent<'a>(
        &'a self,
        place: &'a Place,
        before: &'a str,
    ) -> BoxFuture<'a, Result<Vec<Earlier>, HistoryError>> {
        let channels = self.channels.lock().unwrap();
        let messages = channels.get(&place.channel_id).cloned().unwrap_or_default();
        let read = messages
            .into_iter()
            .filter(|message| message.message_id.as_str() < before)
            .collect();
        Box::pin(async move { Ok(read) })
    }

    fn link<'a>(&'a self, place: &'a Place) -> BoxFuture<'a, Option<String>> {
        let url = format!(
            "https://example.slack.com/archives/{}/p{}",
            place.channel_id,
            place.thread_id.replace('.', "")
        );
        Box::pin(async move { Some(url) })
    }

    fn channel_name<'a>(&'a self, place: &'a Place) -> BoxFuture<'a, Option<String>> {
        let name = format!("{}-name", place.channel_id);
        Box::pin(async move { Some(name) })
    }
}
