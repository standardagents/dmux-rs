//! ChatGPT-subscription inference through `codex app-server` (port of
//! `CodexAppServerClient.generateText`): line-delimited JSON-RPC over the
//! child's stdio — initialize, thread/start (ephemeral, read-only, no tools),
//! turn/start, then collect agentMessage items until turn/completed.

use std::process::Stdio;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

use crate::InferError;

pub async fn generate(model: &str, system: &str, user: &str) -> Result<String, InferError> {
    let result = tokio::time::timeout(std::time::Duration::from_secs(30), run(model, system, user)).await;
    match result {
        Ok(inner) => inner,
        Err(_) => Err(InferError::Request("codex app-server timed out".into())),
    }
}

async fn run(model: &str, system: &str, user: &str) -> Result<String, InferError> {
    let mut child = Command::new("codex")
        .args(["app-server"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| InferError::Request(format!("codex app-server spawn: {e}")))?;

    let mut stdin = child.stdin.take().expect("piped stdin");
    let stdout = child.stdout.take().expect("piped stdout");
    let mut lines = BufReader::new(stdout).lines();
    let mut next_id: u64 = 0;

    // Helper: send a request and await its reply (replies carry our id;
    // notifications don't).
    async fn round_trip(
        stdin: &mut tokio::process::ChildStdin,
        lines: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
        id: u64,
        method: &str,
        params: Value,
    ) -> Result<Value, InferError> {
        let msg = json!({"method": method, "id": id, "params": params});
        stdin
            .write_all(format!("{msg}\n").as_bytes())
            .await
            .map_err(|e| InferError::Request(e.to_string()))?;
        while let Some(line) = lines.next_line().await.map_err(|e| InferError::Request(e.to_string()))? {
            let v: Value = match serde_json::from_str(&line) {
                Ok(v) => v,
                Err(_) => continue,
            };
            if v["id"].as_u64() == Some(id) {
                if !v["error"].is_null() {
                    let msg = v["error"]["message"].as_str().unwrap_or("rpc error");
                    return Err(InferError::Request(format!("{method}: {msg}")));
                }
                return Ok(v["result"].clone());
            }
        }
        Err(InferError::Request(format!("{method}: stream closed")))
    }

    next_id += 1;
    round_trip(&mut stdin, &mut lines, next_id, "initialize", json!({
        "clientInfo": {"name": "dmux", "title": "dmux", "version": "1"}
    }))
    .await?;
    stdin
        .write_all(b"{\"method\":\"initialized\",\"params\":{}}\n")
        .await
        .map_err(|e| InferError::Request(e.to_string()))?;

    next_id += 1;
    let thread = round_trip(&mut stdin, &mut lines, next_id, "thread/start", json!({
        "model": model,
        "cwd": std::env::temp_dir().to_string_lossy(),
        "approvalPolicy": "never",
        "sandbox": "read-only",
        "serviceName": "dmux_inference",
        "ephemeral": true,
        "baseInstructions": "You are a text inference service. Never use tools. Return only the requested answer.",
        "developerInstructions": system,
    }))
    .await?;
    let thread_id = thread["thread"]["id"]
        .as_str()
        .ok_or_else(|| InferError::BadResponse("no thread id".into()))?
        .to_string();

    next_id += 1;
    let turn_msg = json!({
        "method": "turn/start",
        "id": next_id,
        "params": {"threadId": thread_id, "input": [{"type": "text", "text": user}]}
    });
    stdin
        .write_all(format!("{turn_msg}\n").as_bytes())
        .await
        .map_err(|e| InferError::Request(e.to_string()))?;

    // Collect notifications until the turn completes.
    let mut final_text = String::new();
    while let Some(line) = lines.next_line().await.map_err(|e| InferError::Request(e.to_string()))? {
        let v: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if v["params"]["threadId"].as_str() != Some(thread_id.as_str()) {
            continue;
        }
        match v["method"].as_str() {
            Some("item/completed") if v["params"]["item"]["type"].as_str() == Some("agentMessage") => {
                if let Some(text) = v["params"]["item"]["text"].as_str() {
                    final_text = text.to_string();
                }
            }
            Some("turn/completed") => {
                let turn = &v["params"]["turn"];
                match turn["status"].as_str() {
                    Some("failed") => {
                        let msg = turn["error"]["message"].as_str().unwrap_or("ChatGPT inference failed");
                        return Err(InferError::Request(msg.to_string()));
                    }
                    _ => {
                        if let Some(items) = turn["items"].as_array() {
                            if let Some(text) = items.iter().rev().find_map(|i| {
                                (i["type"].as_str() == Some("agentMessage"))
                                    .then(|| i["text"].as_str())
                                    .flatten()
                            }) {
                                final_text = text.to_string();
                            }
                        }
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    let _ = child.start_kill();
    let trimmed = final_text.trim().to_string();
    if trimmed.is_empty() {
        Err(InferError::BadResponse("ChatGPT returned an empty response".into()))
    } else {
        Ok(trimmed)
    }
}
