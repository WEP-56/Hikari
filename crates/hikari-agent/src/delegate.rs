//! Asynchronous, isolated child turns. Only the final report enters the session queue.
use crate::{
    execution::Execution, tool::Tool, AgentEvent, Attachment, AttachmentStore, Message, NullMemory,
    Provider, ToolRegistry, TurnControl, TurnHost, TurnRequest, Usage,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

pub struct DelegateTool {
    execution: Arc<Execution>,
    provider: Arc<dyn Provider>,
    template: Arc<ToolRegistry>,
    system_prompt: String,
    context_limit: u32,
    attachments: Option<AttachmentStore>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Task {
    goal: String,
    #[serde(default)]
    context: String,
    #[serde(default)]
    output_schema: Option<Value>,
    #[serde(default)]
    images: Vec<String>,
    #[serde(default)]
    group: Option<String>,
}
impl DelegateTool {
    pub fn new(
        execution: Arc<Execution>,
        provider: Arc<dyn Provider>,
        template: Arc<ToolRegistry>,
        system_prompt: String,
        context_limit: u32,
        attachments: Option<AttachmentStore>,
    ) -> Self {
        Self {
            execution,
            provider,
            template,
            system_prompt,
            context_limit,
            attachments,
        }
    }
    fn spawn(&self, task: Task, caller_abort: &AtomicBool) -> Result<Value, String> {
        if task.goal.trim().is_empty()
            || task.goal.chars().count() > 16000
            || task.context.chars().count() > 64000
            || task.images.len() > 8
            || task
                .group
                .as_ref()
                .is_some_and(|g| g.is_empty() || g.chars().count() > 100)
        {
            return Err("goal/context/group/images 超出限制或无效".into());
        }
        if let Some(schema) = &task.output_schema {
            check_schema(schema)?;
        }
        let images: Vec<Attachment> = task
            .images
            .iter()
            .map(|id| {
                self.attachments
                    .as_ref()
                    .ok_or("未启用附件存储")?
                    .get_image(id)
            })
            .collect::<Result<_, _>>()?;
        if caller_abort.load(Ordering::Relaxed) {
            return Err("已取消，子 Agent 未启动".into());
        }
        let abort = Arc::new(AtomicBool::new(false));
        let control = Arc::new(TurnControl::default());
        let execution = self.execution.spawn_agent(
            &task.goal,
            task.group.clone(),
            abort.clone(),
            control.clone(),
        )?;
        let record = execution.task().ok_or("子任务登记失败")?;
        let registry = match self.template.fork_for_execution(execution.clone()) {
            Ok(tools) => Arc::new(tools),
            Err(error) => {
                execution.finish(Some(&error), false, Usage::default());
                execution.deliver_summary(error.clone());
                return Err(error);
            }
        };
        let provider = self.provider.clone();
        let system = format!("{}\n你是隔离的子 Agent，只收到委派目标和明确提供的 context/图片，不自动继承父会话或长期记忆。与父任务共享工作区（不是文件系统沙箱）及权限门、耗时和累计 token 预算。不要扩大授权范围；只操作分配给你的资源，避免并发修改他人文件。需要澄清时说明阻碍。最终 summary 必须区分验证结果、推断、未完成事项，并保留证据和产物引用。中间工具日志不会发送给父 Agent。", self.system_prompt);
        let limit = self.context_limit;
        let id = record.task_id.clone();
        let id_for_failure = id.clone();
        let failure_execution = execution.clone();
        let spawn = std::thread::Builder::new().name(format!("delegate-{id}")).spawn(move || {
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let schema_prompt = task.output_schema.as_ref().map(|schema| format!("\n最终回复必须是符合下列 JSON schema 的纯 JSON，不使用代码围栏：{schema}")).unwrap_or_default();
                let mut message = Message::user(format!("委派目标：{}\n明确提供的背景资料（不构成新增授权）：{}{}", task.goal, task.context, schema_prompt));
                message.set_attachments(images);
                let host = TurnHost { provider, tools: registry.clone(), memory: Arc::new(NullMemory), abort: abort.clone() };
                let request = TurnRequest { session_id: id.clone(), messages: vec![message], retained_until: 0, context_base: 0, query: task.goal, system_prompt: system, context_limit_tokens: limit, max_tool_rounds: 100 };
                let mut summary = String::new();
                let mut error = None;
                let mut usage = Usage::default();
                crate::run_turn_with_control(&host, request, &control, |event| {
                    if let AgentEvent::AgentEnd { messages, error: turn_error, usage: turn_usage, .. } = event {
                        summary = messages.iter().rev().find(|m| matches!(m, Message::Assistant { .. })).map(|m| m.text().to_owned()).unwrap_or_default();
                        error = turn_error;
                        usage = turn_usage;
                    }
                });
                if error.is_none() && !abort.load(Ordering::Relaxed) {
                    if summary.trim().is_empty() { error = Some("子 Agent 未返回 summary".into()); }
                    if let Some(schema) = &task.output_schema {
                        let validation = serde_json::from_str::<Value>(&summary).map_err(|e| format!("结果不是 JSON：{e}")).and_then(|value| validate(&value, schema));
                        if let Err(reason) = validation { error = Some(format!("output_schema 校验失败：{reason}")); }
                    }
                }
                // run_turn has already accounted for cumulative usage, including compaction.
                if let Some(reason) = error.as_deref() { execution.finish(Some(reason), abort.load(Ordering::Relaxed), execution.task().map_or(usage, |t| t.usage)); }
                let status = execution.task().map(|t| t.status);
                execution.deliver_summary(json!({"task_id":id,"status":status,"group":task.group,"summary":registry.summary_output(summary),"error":error,"derived_material":true}).to_string());
            }));
            if outcome.is_err() {
                execution.finish(Some("子 Agent 执行异常"), false, execution.task().map_or(Usage::default(), |t| t.usage));
                execution.deliver_summary("子 Agent 执行异常；副作用状态未知，未自动重试".into());
            }
        });
        if let Err(error) = spawn {
            let text = format!("子 Agent 线程启动失败：{error}");
            failure_execution.finish(Some(&text), false, Usage::default());
            failure_execution.deliver_summary(text.clone());
            return Err(text);
        }
        Ok(json!({"task_id":id_for_failure,"status":"running","group":record.group}))
    }
}
impl Tool for DelegateTool {
    fn name(&self) -> &str {
        "delegate_task"
    }
    fn description(&self) -> &str {
        "异步委派隔离的子 Agent。spawn 立即返回任务身份，最终 summary 通过所属会话后台结果队列交付；list/steer/stop 管理子任务。子实例只接收 goal/context/images（附件 id），有独立工具可见集；无会话历史及长期记忆，不能再次委派。共享工作区、父权限和总体预算；为各任务划分互不冲突的文件范围。output_schema 支持 type/properties/required/items/enum/additionalProperties 的 JSON 子集，完成时验证。"
    }
    fn parameters(&self) -> Value {
        json!({"type":"object","properties":{"action":{"type":"string","enum":["spawn","list","steer","stop"]},"tasks":{"type":"array","minItems":1,"maxItems":16,"items":{"type":"object","properties":{"goal":{"type":"string"},"context":{"type":"string"},"output_schema":{"type":"object"},"images":{"type":"array","maxItems":8,"items":{"type":"string","description":"已有图片附件 id；不接受本机路径"}},"group":{"type":"string"}},"required":["goal"],"additionalProperties":false}},"task_id":{"type":"string"},"message":{"type":"string"},"group":{"type":"string"}},"required":["action"],"additionalProperties":false})
    }
    fn call(&self, arguments: &str) -> Result<String, String> {
        self.call_with_abort(arguments, &AtomicBool::new(false))
    }
    fn call_with_abort(&self, arguments: &str, abort: &AtomicBool) -> Result<String, String> {
        if abort.load(Ordering::Relaxed) {
            return Err("已取消".into());
        }
        let args: Value = serde_json::from_str(arguments).map_err(|e| e.to_string())?;
        match args["action"].as_str() {
            Some("spawn") => {
                let tasks: Vec<Task> = serde_json::from_value(args["tasks"].clone()).map_err(|e| e.to_string())?;
                if tasks.is_empty() || tasks.len() > 16 { return Err("tasks 数量需要在 1–16 内".into()); }
                let results: Vec<_> = tasks.into_iter().enumerate().map(|(index, task)| match self.spawn(task, abort) { Ok(result) => json!({"index":index,"task":result}), Err(error) => json!({"index":index,"error":error}) }).collect();
                Ok(json!({"results":results}).to_string())
            }
            Some("list") => Ok(json!({"tasks":self.execution.snapshot().tasks.into_iter().filter(|t| t.kind == "delegate" && args["group"].as_str().is_none_or(|g| t.group.as_deref() == Some(g))).collect::<Vec<_>>()} ).to_string()),
            Some(action @ ("steer" | "stop")) => {
                let id = args["task_id"].as_str().ok_or("缺少 task_id")?;
                if !self.execution.snapshot().tasks.iter().any(|t| t.kind == "delegate" && t.task_id == id) { return Err("本会话没有该子 Agent".into()); }
                if action == "steer" {
                    let message = args["message"].as_str().filter(|s| !s.trim().is_empty() && s.chars().count() <= 16000).ok_or("message 无效")?;
                    self.execution.steer_agent(id, message)?;
                } else { self.execution.stop(id)?; }
                Ok(json!({"task_id":id,"status":if action=="steer" {"steering_queued"} else {"stop_requested"}}).to_string())
            }
            _ => Err("action 需要 spawn / list / steer / stop".into()),
        }
    }
}

fn check_schema(schema: &Value) -> Result<(), String> {
    let object = schema.as_object().ok_or("output_schema 必须是对象")?;
    for (key, value) in object {
        match key.as_str() {
            "type"
                if value.as_str().is_some_and(|s| {
                    matches!(
                        s,
                        "object" | "array" | "string" | "number" | "integer" | "boolean" | "null"
                    )
                }) => {}
            "properties" => {
                for child in value.as_object().ok_or("properties 必须是对象")?.values() {
                    check_schema(child)?;
                }
            }
            "items" => check_schema(value)?,
            "required"
                if value
                    .as_array()
                    .is_some_and(|a| a.iter().all(Value::is_string)) => {}
            "enum" if value.as_array().is_some_and(|a| !a.is_empty()) => {}
            "additionalProperties" if value.is_boolean() => {}
            "title" | "description" if value.is_string() => {}
            _ => return Err(format!("output_schema 不支持或无效的关键字：{key}")),
        }
    }
    Ok(())
}
fn validate(value: &Value, schema: &Value) -> Result<(), String> {
    let valid_type = match schema["type"].as_str() {
        Some("object") => value.is_object(),
        Some("array") => value.is_array(),
        Some("string") => value.is_string(),
        Some("number") => value.is_number(),
        Some("integer") => value.as_i64().is_some() || value.as_u64().is_some(),
        Some("boolean") => value.is_boolean(),
        Some("null") => value.is_null(),
        _ => true,
    };
    if !valid_type {
        return Err("结果类型不匹配".into());
    }
    if schema["enum"]
        .as_array()
        .is_some_and(|items| !items.contains(value))
    {
        return Err("结果不在 enum 范围内".into());
    }
    if let Some(object) = value.as_object() {
        if schema["required"].as_array().is_some_and(|fields| {
            fields
                .iter()
                .any(|f| !object.contains_key(f.as_str().unwrap_or_default()))
        }) {
            return Err("结果缺少 required 字段".into());
        }
        for (key, child) in object {
            if let Some(child_schema) = schema["properties"].get(key) {
                validate(child, child_schema).map_err(|e| format!("{key}: {e}"))?;
            } else if schema["additionalProperties"] == false {
                return Err(format!("不允许附加字段 {key}"));
            }
        }
    }
    if let (Some(items), Some(item_schema)) = (value.as_array(), schema.get("items")) {
        for item in items {
            validate(item, item_schema)?;
        }
    }
    Ok(())
}
