//! Agent-facing tools for cross-sandbox primitives.
//!
//! Only compiled when the `vms` feature is enabled (which adds the
//! `cersei-vms` dependency). The active sandbox handle, mailbox, and
//! KV store are read from `ToolContext.extensions`.

#![cfg(feature = "vms")]

use super::*;
use cersei_vms::{KvStore, Mailbox, MailboxSubscription, SandboxId};
use serde::Deserialize;
use std::sync::Arc;
use tokio::sync::Mutex as AsyncMutex;

// ─── helpers ────────────────────────────────────────────────────────────────

fn require_sandbox_id(ctx: &ToolContext) -> std::result::Result<SandboxId, ToolResult> {
    match ctx.extensions.get::<Arc<dyn cersei_vms::Sandbox>>() {
        Some(sb) => Ok(sb.id().clone()),
        None => Err(ToolResult::error(
            "no active sandbox in this tool context",
        )),
    }
}

fn require_mailbox(ctx: &ToolContext) -> std::result::Result<Mailbox, ToolResult> {
    ctx.extensions
        .get::<Mailbox>()
        .map(|m| (*m).clone())
        .ok_or_else(|| ToolResult::error("no Mailbox registered in tool context"))
}

fn require_kv(ctx: &ToolContext) -> std::result::Result<KvStore, ToolResult> {
    ctx.extensions
        .get::<KvStore>()
        .map(|kv| (*kv).clone())
        .ok_or_else(|| ToolResult::error("no KvStore registered in tool context"))
}

// ─── SendVmMessage ──────────────────────────────────────────────────────────

pub struct SendVmMessageTool;

#[async_trait]
impl Tool for SendVmMessageTool {
    fn name(&self) -> &str {
        "SendVmMessage"
    }
    fn description(&self) -> &str {
        "Publish a JSON message to a cross-sandbox topic. Other agents subscribed to the same topic will receive it."
    }
    fn permission_level(&self) -> PermissionLevel {
        PermissionLevel::Write
    }
    fn category(&self) -> ToolCategory {
        ToolCategory::Orchestration
    }
    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "topic": { "type": "string", "description": "Topic to publish to (e.g. 'workers/results')" },
                "payload": { "description": "JSON payload" }
            },
            "required": ["topic", "payload"]
        })
    }
    async fn execute(&self, input: Value, ctx: &ToolContext) -> ToolResult {
        #[derive(Deserialize)]
        struct Input {
            topic: String,
            payload: Value,
        }
        let Input { topic, payload } = match serde_json::from_value(input) {
            Ok(i) => i,
            Err(e) => return ToolResult::error(format!("Invalid input: {e}")),
        };
        let mailbox = match require_mailbox(ctx) {
            Ok(m) => m,
            Err(r) => return r,
        };
        let from = match require_sandbox_id(ctx) {
            Ok(id) => id,
            Err(_) => SandboxId::from("host"),
        };
        match mailbox.publish(topic.clone(), from, payload) {
            Ok(seq) => ToolResult::success(format!("published seq={seq} topic={topic}")),
            Err(e) => ToolResult::error(format!("publish failed: {e}")),
        }
    }
}

// ─── RecvVmMessage ──────────────────────────────────────────────────────────
//
// Subscriptions are stateful — we cache them in extensions keyed by
// (sandbox_id, topic). The agent calls `RecvVmMessage` repeatedly to drain.

#[derive(Clone, Default)]
struct SubscriptionRegistry {
    subs: Arc<
        parking_lot::Mutex<
            std::collections::HashMap<(String, String), Arc<AsyncMutex<MailboxSubscription>>>,
        >,
    >,
}

pub struct RecvVmMessageTool;

#[async_trait]
impl Tool for RecvVmMessageTool {
    fn name(&self) -> &str {
        "RecvVmMessage"
    }
    fn description(&self) -> &str {
        "Receive the next message on a cross-sandbox topic. Blocks until a message arrives or the optional timeout elapses."
    }
    fn permission_level(&self) -> PermissionLevel {
        PermissionLevel::ReadOnly
    }
    fn category(&self) -> ToolCategory {
        ToolCategory::Orchestration
    }
    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "topic": { "type": "string", "description": "Topic to receive from" },
                "timeout_ms": { "type": "integer", "description": "How long to wait (default 5000, max 60000)" }
            },
            "required": ["topic"]
        })
    }
    async fn execute(&self, input: Value, ctx: &ToolContext) -> ToolResult {
        #[derive(Deserialize)]
        struct Input {
            topic: String,
            timeout_ms: Option<u64>,
        }
        let Input { topic, timeout_ms } = match serde_json::from_value(input) {
            Ok(i) => i,
            Err(e) => return ToolResult::error(format!("Invalid input: {e}")),
        };
        let mailbox = match require_mailbox(ctx) {
            Ok(m) => m,
            Err(r) => return r,
        };
        let registry = match ctx.extensions.get::<SubscriptionRegistry>() {
            Some(r) => (*r).clone(),
            None => {
                let r = SubscriptionRegistry::default();
                ctx.extensions.insert(r.clone());
                r
            }
        };
        let sandbox_key = require_sandbox_id(ctx)
            .map(|id| id.to_string())
            .unwrap_or_else(|_| "host".into());
        let key = (sandbox_key, topic.clone());
        let sub = {
            let mut subs = registry.subs.lock();
            subs.entry(key)
                .or_insert_with(|| Arc::new(AsyncMutex::new(mailbox.subscribe(topic.clone()))))
                .clone()
        };
        let dur = std::time::Duration::from_millis(timeout_ms.unwrap_or(5_000).min(60_000));
        let fut = async {
            let mut guard = sub.lock().await;
            guard.recv().await
        };
        match tokio::time::timeout(dur, fut).await {
            Ok(Ok(env)) => match serde_json::to_string(&env) {
                Ok(s) => ToolResult::success(s),
                Err(e) => ToolResult::error(format!("serialize envelope: {e}")),
            },
            Ok(Err(e)) => ToolResult::error(format!("mailbox: {e}")),
            Err(_) => ToolResult::error("timeout waiting for mailbox message"),
        }
    }
}

// ─── SharedStateGet / SharedStateSet ────────────────────────────────────────

pub struct SharedStateGetTool;

#[async_trait]
impl Tool for SharedStateGetTool {
    fn name(&self) -> &str {
        "SharedStateGet"
    }
    fn description(&self) -> &str {
        "Read a value from the cross-sandbox key/value store. Returns the value as a UTF-8 string, the version, and updated_at."
    }
    fn permission_level(&self) -> PermissionLevel {
        PermissionLevel::ReadOnly
    }
    fn category(&self) -> ToolCategory {
        ToolCategory::Memory
    }
    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": { "key": { "type": "string" } },
            "required": ["key"]
        })
    }
    async fn execute(&self, input: Value, ctx: &ToolContext) -> ToolResult {
        #[derive(Deserialize)]
        struct Input {
            key: String,
        }
        let Input { key } = match serde_json::from_value(input) {
            Ok(i) => i,
            Err(e) => return ToolResult::error(format!("Invalid input: {e}")),
        };
        let kv = match require_kv(ctx) {
            Ok(kv) => kv,
            Err(r) => return r,
        };
        match kv.get(&key) {
            Some(entry) => {
                let body = serde_json::json!({
                    "key": key,
                    "value": String::from_utf8_lossy(&entry.value),
                    "version": entry.version,
                    "updated_at_unix_ms": entry.updated_at_unix_ms,
                });
                ToolResult::success(body.to_string())
            }
            None => ToolResult::error(format!("key not found: {key}")),
        }
    }
}

pub struct SharedStateSetTool;

#[async_trait]
impl Tool for SharedStateSetTool {
    fn name(&self) -> &str {
        "SharedStateSet"
    }
    fn description(&self) -> &str {
        "Write a UTF-8 value to the cross-sandbox key/value store. Optional expected_version enables compare-and-swap."
    }
    fn permission_level(&self) -> PermissionLevel {
        PermissionLevel::Write
    }
    fn category(&self) -> ToolCategory {
        ToolCategory::Memory
    }
    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "key": { "type": "string" },
                "value": { "type": "string" },
                "expected_version": { "type": "integer", "description": "If set, only update when current version matches" }
            },
            "required": ["key", "value"]
        })
    }
    async fn execute(&self, input: Value, ctx: &ToolContext) -> ToolResult {
        #[derive(Deserialize)]
        struct Input {
            key: String,
            value: String,
            expected_version: Option<u64>,
        }
        let Input {
            key,
            value,
            expected_version,
        } = match serde_json::from_value(input) {
            Ok(i) => i,
            Err(e) => return ToolResult::error(format!("Invalid input: {e}")),
        };
        let kv = match require_kv(ctx) {
            Ok(kv) => kv,
            Err(r) => return r,
        };
        let bytes = value.into_bytes();
        if let Some(ev) = expected_version {
            match kv.cas(&key, Some(ev), bytes) {
                Ok(Some(entry)) => ToolResult::success(format!("ok version={}", entry.version)),
                Ok(None) => ToolResult::error("cas failed: version mismatch"),
                Err(e) => ToolResult::error(format!("cas error: {e}")),
            }
        } else {
            match kv.set(key.clone(), bytes) {
                Ok(entry) => ToolResult::success(format!("ok version={}", entry.version)),
                Err(e) => ToolResult::error(format!("set error: {e}")),
            }
        }
    }
}

// ─── Snapshot tool ──────────────────────────────────────────────────────────

pub struct SandboxSnapshotTool;

#[async_trait]
impl Tool for SandboxSnapshotTool {
    fn name(&self) -> &str {
        "SandboxSnapshot"
    }
    fn description(&self) -> &str {
        "Take a snapshot of the current sandbox (filesystem + KV state) and return the snapshot id."
    }
    fn permission_level(&self) -> PermissionLevel {
        PermissionLevel::Write
    }
    fn category(&self) -> ToolCategory {
        ToolCategory::Orchestration
    }
    fn input_schema(&self) -> Value {
        serde_json::json!({ "type": "object", "properties": {} })
    }
    async fn execute(&self, _input: Value, ctx: &ToolContext) -> ToolResult {
        let sandbox = match ctx.extensions.get::<Arc<dyn cersei_vms::Sandbox>>() {
            Some(s) => s,
            None => return ToolResult::error("no active sandbox"),
        };
        match sandbox.snapshot().await {
            Ok(id) => ToolResult::success(format!("snapshot_id={id}")),
            Err(e) => ToolResult::error(format!("snapshot failed: {e}")),
        }
    }
}

/// All `vms`-feature tools as boxed `Tool` trait objects.
pub fn all_vm_tools() -> Vec<Box<dyn Tool>> {
    vec![
        Box::new(SendVmMessageTool),
        Box::new(RecvVmMessageTool),
        Box::new(SharedStateGetTool),
        Box::new(SharedStateSetTool),
        Box::new(SandboxSnapshotTool),
    ]
}
