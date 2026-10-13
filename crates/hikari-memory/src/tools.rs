use crate::*;
use hikari_agent::tool::Tool;

pub fn register(scope: Arc<Scope>, registry: &mut ToolRegistry) -> Result<()> {
    if scope.enabled() {
        for name in ["search_observations", "recall", "expand", "forget"] {
            registry
                .register(Arc::new(MemoryTool {
                    scope: scope.clone(),
                    name,
                }))
                .map_err(error)?;
        }
    }
    Ok(())
}
struct MemoryTool {
    scope: Arc<Scope>,
    name: &'static str,
}
impl Tool for MemoryTool {
    fn name(&self) -> &str {
        self.name
    }
    fn description(&self) -> &str {
        match self.name {
            "search_observations" => "搜索长期观察和偏好，默认最多 5 项 / 2000 tokens。过时或不足时再 recall。",
            "recall" => "搜索 world/experience 原始事实，默认最多 5 项 / 3000 tokens。可换词补查。",
            "expand" => "按事实 ID 展开来源 chunk/document，最多 5000 tokens，输出可能截断。",
            _ => "仅在用户明确要求遗忘时使用。先用读工具定位明确目标，用 memory_ids 删除相关事实和派生观察；不能自行遗忘。",
        }
    }
    fn parameters(&self) -> Value {
        let mut properties = json!({"query":{"type":"string"},"max_tokens":{"type":"integer","minimum":128},"limit":{"type":"integer","minimum":1,"maximum":5},"fact_types":{"type":"array","items":{"type":"string","enum":["world","experience"]}},"occurred_after":{"type":"string"},"occurred_before":{"type":"string"}});
        let required = match self.name {
            "expand" => {
                properties = json!({"memory_ids":{"type":"array","minItems":1,"maxItems":5,"items":{"type":"string"}},"depth":{"type":"string","enum":["chunk","document"]},"max_tokens":{"type":"integer","minimum":128,"maximum":5000}});
                vec!["memory_ids", "depth"]
            }
            "forget" => {
                properties = json!({"query":{"type":"string"},"memory_ids":{"type":"array","minItems":1,"maxItems":20,"items":{"type":"string"}}});
                vec!["query", "memory_ids"]
            }
            _ => vec!["query"],
        };
        json!({"type":"object","properties":properties,"required":required,"additionalProperties":false})
    }
    fn output_limit(&self) -> usize {
        100_000
    }
    fn call(&self, arguments: &str) -> std::result::Result<String, String> {
        let call = || -> Result<Value> {
            if !self.scope.enabled() {
                return Err(error("记忆已关闭"));
            }
            let args: Value = serde_json::from_str(arguments).map_err(error)?;
            let object = args.as_object().ok_or_else(|| error("参数必须是对象"))?;
            let allowed = match self.name {
                "expand" => vec!["memory_ids", "depth", "max_tokens"],
                "forget" => vec!["query", "memory_ids"],
                _ => vec![
                    "query",
                    "max_tokens",
                    "limit",
                    "fact_types",
                    "occurred_after",
                    "occurred_before",
                ],
            };
            if object.keys().any(|k| !allowed.contains(&k.as_str())) {
                return Err(error("包含未知记忆工具参数"));
            }
            for key in ["limit", "max_tokens"] {
                if object.get(key).is_some_and(|v| v.as_u64().is_none()) {
                    return Err(error("limit/max_tokens 必须为非负整数"));
                }
            }
            if args
                .get("max_tokens")
                .and_then(Value::as_u64)
                .is_some_and(|n| n < 128)
            {
                return Err(error("max_tokens 至少为 128"));
            }
            if args
                .get("limit")
                .and_then(Value::as_u64)
                .is_some_and(|n| n == 0)
            {
                return Err(error("limit 至少为 1"));
            }
            for key in ["query", "depth", "occurred_after", "occurred_before"] {
                if object.get(key).is_some_and(|v| !v.is_string()) {
                    return Err(error("查询、深度与日期必须为字符串"));
                }
            }
            let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(5) as usize;
            let cap = match self.name {
                "search_observations" => 2000,
                "recall" => 3000,
                _ => 5000,
            };
            let tokens = args
                .get("max_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(cap) as usize;
            let query = args
                .get("query")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim();
            let ids = || -> Result<Vec<String>> {
                serde_json::from_value(
                    args.get("memory_ids")
                        .cloned()
                        .ok_or_else(|| error("需要 memory_ids"))?,
                )
                .map_err(error)
            };
            match self.name {
                "expand" => self.scope.expand(
                    &ids()?,
                    args.get("depth").and_then(Value::as_str).unwrap_or("chunk"),
                    tokens.min(cap as usize),
                ),
                "forget" => self.scope.forget(query, &ids()?),
                _ => {
                    if query.is_empty() {
                        return Err(error("query 不能为空"));
                    }
                    let requested: Vec<String> = args
                        .get("fact_types")
                        .map(|v| serde_json::from_value(v.clone()))
                        .transpose()
                        .map_err(error)?
                        .unwrap_or_else(|| vec!["world".into(), "experience".into()]);
                    if requested
                        .iter()
                        .any(|k| !matches!(k.as_str(), "world" | "experience"))
                    {
                        return Err(error("fact_types 只支持 world/experience"));
                    }
                    let kinds: Vec<&str> = if self.name == "search_observations" {
                        vec!["observation"]
                    } else {
                        requested.iter().map(String::as_str).collect()
                    };
                    let start = args.get("occurred_after").and_then(Value::as_str);
                    let end = args.get("occurred_before").and_then(Value::as_str);
                    for t in [start, end].into_iter().flatten() {
                        DateTime::parse_from_rfc3339(t).map_err(error)?;
                    }
                    self.scope
                        .query(query, &kinds, limit, tokens.min(cap as usize), start, end)
                }
            }
        };
        call().map(|v| v.to_string()).map_err(|e| e.message)
    }
}
