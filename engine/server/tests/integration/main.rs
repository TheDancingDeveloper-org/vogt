//! End-to-end integration tests: start the real Axum server on an OS-assigned
//! port, talk to it over HTTP + WebSocket the same way a client would.

#[path = "../common/mod.rs"]
mod common;
#[allow(unused_imports)]
use common::*;

mod agent_clis;
mod agent_tasks;
mod attach;
mod chats;
mod files_git;
mod hibernation;
mod history;
mod http;
mod llm;
mod pod_stream;
mod push;
mod runs;
mod screen;
mod secrets;
mod sessions;
