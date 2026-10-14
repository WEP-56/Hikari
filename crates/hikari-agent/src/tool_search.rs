//! Local discovery. Loading changes the visible tool set, never execution permissions.
use crate::ToolDef;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;

pub fn definition() -> ToolDef {
    ToolDef { name: "tool_search".into(), description: "搜索尚未加载的 MCP 工具并在下一次请求中启用命中工具。支持工具名称、用途、参数及服务信息；MCP 能力先通过此工具发现。".into(), parameters: json!({"type":"object","properties":{"query":{"type":"string"},"limit":{"type":"integer","minimum":1,"maximum":16}},"required":["query"],"additionalProperties":false}) }
}

pub fn tokenize(text: &str) -> Vec<String> {
    let mut normalized = String::new();
    let mut previous = ' ';
    for ch in text.chars() {
        if ch.is_uppercase() && (previous.is_lowercase() || previous.is_ascii_digit()) {
            normalized.push(' ');
        }
        normalized.extend(ch.to_lowercase());
        previous = ch;
    }
    let mut terms = Vec::new();
    for word in normalized.split(|c: char| !c.is_alphanumeric()) {
        let mut ascii = String::new();
        let mut cjk = Vec::new();
        let flush = |ascii: &mut String, cjk: &mut Vec<char>, terms: &mut Vec<String>| {
            if !ascii.is_empty() {
                terms.push(std::mem::take(ascii));
            }
            terms.extend(cjk.iter().map(char::to_string));
            terms.extend(cjk.windows(2).map(|w| w.iter().collect()));
            cjk.clear();
        };
        for c in word.chars() {
            if c.is_ascii() {
                if !cjk.is_empty() {
                    flush(&mut ascii, &mut cjk, &mut terms);
                }
                ascii.push(c);
            } else {
                if !ascii.is_empty() {
                    flush(&mut ascii, &mut cjk, &mut terms);
                }
                cjk.push(c);
            }
        }
        flush(&mut ascii, &mut cjk, &mut terms);
    }
    terms
}

fn schema_text(value: &Value, out: &mut String) {
    if let Some(description) = value["description"].as_str() {
        out.push(' ');
        out.push_str(description);
    }
    if let Some(properties) = value["properties"].as_object() {
        for (name, property) in properties {
            out.push(' ');
            out.push_str(name);
            schema_text(property, out);
        }
    }
    if let Some(items) = value.get("items") {
        schema_text(items, out);
    }
    for key in ["anyOf", "oneOf", "allOf"] {
        if let Some(items) = value[key].as_array() {
            for item in items {
                schema_text(item, out);
            }
        }
    }
}

pub fn search(
    defs: &[ToolDef],
    deferred: &BTreeMap<String, String>,
    loaded: &Mutex<Vec<String>>,
    arguments: &str,
) -> Result<String, String> {
    let args: Value = serde_json::from_str(arguments).map_err(|e| e.to_string())?;
    let query = args["query"]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .ok_or("query 不能为空")?;
    let limit = match args.get("limit") {
        None => 8,
        Some(v) => v
            .as_u64()
            .filter(|n| (1..=16).contains(n))
            .ok_or("limit 需要在 1–16 内")? as usize,
    };
    let terms: BTreeSet<_> = tokenize(query).into_iter().collect();
    let mut active = loaded.lock().map_err(|_| "工具加载状态不可用")?;
    let candidates: Vec<_> = defs
        .iter()
        .filter(|d| deferred.contains_key(&d.name) && !active.contains(&d.name))
        .collect();
    let docs: Vec<BTreeMap<String, usize>> = candidates
        .iter()
        .map(|d| {
            let mut text = format!("{} {} {}", d.name, d.description, deferred[&d.name]);
            schema_text(&d.parameters, &mut text);
            let mut counts = BTreeMap::new();
            for term in tokenize(&text) {
                *counts.entry(term).or_insert(0) += 1;
            }
            counts
        })
        .collect();
    let lengths: Vec<usize> = docs.iter().map(|d| d.values().sum()).collect();
    let average = (lengths.iter().sum::<usize>() as f64 / docs.len().max(1) as f64).max(1.0);
    let idfs: BTreeMap<_, _> = terms
        .iter()
        .map(|term| {
            let frequency = docs.iter().filter(|d| d.contains_key(term)).count() as f64;
            (
                term,
                (1.0 + (docs.len() as f64 - frequency + 0.5) / (frequency + 0.5)).ln(),
            )
        })
        .collect();
    let mut scores: Vec<_> = docs
        .iter()
        .enumerate()
        .map(|(i, doc)| {
            let score = terms
                .iter()
                .map(|term| {
                    let count = *doc.get(term).unwrap_or(&0) as f64;
                    let idf = idfs[term];
                    idf * count * 2.2 / (count + 1.2 * (0.25 + 0.75 * lengths[i] as f64 / average))
                })
                .sum::<f64>();
            (i, score)
        })
        .filter(|(_, score)| *score > 0.0)
        .collect();
    scores.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    let matched = !scores.is_empty();
    let mut budget = 6000usize;
    let mut selected = Vec::new();
    for (i, _) in scores {
        let cost = serde_json::to_vec(candidates[i])
            .map_err(|e| e.to_string())?
            .len()
            .div_ceil(3);
        if cost > budget {
            continue;
        }
        budget -= cost;
        active.push(candidates[i].name.clone());
        selected.push(json!({"name":candidates[i].name,"description":candidates[i].description}));
        if selected.len() >= limit {
            break;
        }
    }
    Ok(json!({"status":if !selected.is_empty(){"loaded"}else if matched{"schema_budget_exceeded"}else{"no_match"},"tools":selected,"available":"next_model_request"}).to_string())
}
