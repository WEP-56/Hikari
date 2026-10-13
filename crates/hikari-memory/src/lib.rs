pub mod prompts;
mod tools;

use chrono::{DateTime, Utc};
use hikari_agent::memory::{content_hash, is_memory_tool};
use hikari_agent::{
    AgentError, ConsolidateReceipt, Memory, MemoryJobEvent, Message, RecallItem, Result,
    RetainReceipt, ToolRegistry, Usage,
};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashSet};
use std::path::Path;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc::{self, Receiver, Sender},
    Arc, Mutex, RwLock,
};
use std::time::Duration;

fn error(e: impl std::fmt::Display) -> AgentError {
    AgentError::new(e.to_string())
}
fn now() -> String {
    Utc::now().to_rfc3339()
}
fn date_extreme<'a>(values: impl Iterator<Item = &'a str>, latest: bool) -> Result<Option<String>> {
    let mut selected: Option<(DateTime<chrono::FixedOffset>, &'a str)> = None;
    for value in values {
        let date = DateTime::parse_from_rfc3339(value).map_err(error)?;
        if selected
            .as_ref()
            .is_none_or(|(old, _)| if latest { date > *old } else { date < *old })
        {
            selected = Some((date, value));
        }
    }
    Ok(selected.map(|(_, value)| value.to_string()))
}
fn id(prefix: &str) -> String {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    format!(
        "{prefix}-{}-{}",
        Utc::now().timestamp_nanos_opt().unwrap_or_default(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelRef {
    pub provider_id: String,
    pub model_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MemoryConfig {
    pub enabled: bool,
    pub database: String,
    pub extraction: ModelRef,
    pub consolidation: ModelRef,
    pub auto_tokens: usize,
    pub auto_items: usize,
}
impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            database: "memory.db".into(),
            extraction: ModelRef::default(),
            consolidation: ModelRef::default(),
            auto_tokens: 1000,
            auto_items: 5,
        }
    }
}
impl MemoryConfig {
    pub fn validate(&self) -> Result<()> {
        if self.database.trim().is_empty()
            || !(1..=1000).contains(&self.auto_tokens)
            || !(1..=5).contains(&self.auto_items)
        {
            return Err(error("记忆路径不能为空，自动召回最多 1000 tokens / 5 项"));
        }
        for model in [&self.extraction, &self.consolidation] {
            if model.provider_id.is_empty() != model.model_id.is_empty() {
                return Err(error("记忆模型需同时选择提供商和模型"));
            }
        }
        Ok(())
    }
}

/// Auxiliary requests never emit chat events. The stored request snapshot contains no API key.
pub trait Processor: Send + Sync {
    fn call(&self, model: &Value, system: &str, input: &Value) -> Result<(Value, Usage)>;
}

pub struct Core {
    db: Mutex<Connection>,
    enabled: AtomicBool,
    generation: std::sync::atomic::AtomicU64,
    processor: RwLock<Option<Arc<dyn Processor>>>,
    events: Sender<MemoryJobEvent>,
    wake: Sender<()>,
}

#[derive(Clone)]
pub struct Scope {
    core: Arc<Core>,
    session: String,
    workspace: Option<String>,
    config: MemoryConfig,
    extraction: Value,
    consolidation: Value,
    forget_turn: Arc<AtomicBool>,
    generation: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Unit {
    pub id: String,
    pub text: String,
    pub fact_type: String,
    pub mentioned_at: String,
    pub occurred_start: Option<String>,
    pub occurred_end: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub entities: Vec<String>,
    pub source_message_ids: Vec<String>,
    pub source_memory_ids: Vec<String>,
    pub document_id: Option<String>,
    pub proof_count: u32,
    pub metadata: Value,
    pub consolidated: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum BrowseView {
    /// Observations and facts that have no surviving derived observation.
    #[default]
    Memory,
    Facts,
}
impl BrowseView {
    fn predicate(self) -> &'static str {
        match self {
            Self::Memory => "(u.kind='observation' OR NOT EXISTS (SELECT 1 FROM units o, json_each(o.data,'$.source_memory_ids') proof WHERE o.kind='observation' AND proof.value=u.id))",
            Self::Facts => "u.kind!='observation'",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Source {
    id: String,
    role: String,
    text: String,
    mentioned_at: String,
    #[serde(default)]
    is_error: bool,
    #[serde(default)]
    recalled: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Fact {
    text: String,
    fact_type: String,
    source_message_ids: Vec<String>,
    #[serde(default)]
    entities: Vec<String>,
    #[serde(default)]
    occurred_start: Option<String>,
    #[serde(default)]
    occurred_end: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Extraction {
    facts: Vec<Fact>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Create {
    text: String,
    source_fact_ids: Vec<String>,
    reason: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Update {
    observation_id: String,
    text: String,
    source_fact_ids: Vec<String>,
    reason: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Delete {
    observation_id: String,
    reason: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Actions {
    creates: Vec<Create>,
    updates: Vec<Update>,
    deletes: Vec<Delete>,
}

impl Core {
    fn extract(
        &self,
        processor: &Arc<dyn Processor>,
        job: &str,
        document: &str,
        session: &str,
        payload: &Value,
    ) -> Result<()> {
        let (sources, metadata) = {
            let db = self.db.lock().unwrap();
            let (data, metadata): (String, String) = db
                .query_row(
                    "SELECT sources,metadata FROM documents WHERE id=?",
                    [document],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .map_err(error)?;
            let sources: Vec<Source> = serde_json::from_str(&data).map_err(error)?;
            let sources = sources
                .into_iter()
                .filter(|s| {
                    db.query_row(
                        "SELECT count(*) FROM suppressed_sources WHERE id=?",
                        [&s.id],
                        |r| r.get::<_, i64>(0),
                    )
                    .unwrap_or(1)
                        == 0
                })
                .collect::<Vec<_>>();
            (
                sources,
                serde_json::from_str::<Value>(&metadata).map_err(error)?,
            )
        };
        let mut kept = 0u32;
        let mut used = Usage::default();
        let mut facts = Vec::new();
        // Each input chunk is bounded; IDs always refer to complete original source messages.
        for chunk in sources.chunks(8) {
            let input: Vec<_> = chunk
                .iter()
                .map(|s| {
                    let mut s = s.clone();
                    if s.text.chars().count() > 6000 {
                        s.text = s.text.chars().take(6000).collect::<String>() + "\n[来源已裁剪]";
                    }
                    s
                })
                .collect();
            let (value, usage) = processor.call(
                &payload["model"],
                prompts::EXTRACT,
                &json!({"messages":input}),
            )?;
            used.add(usage);
            let extraction: Extraction = serde_json::from_value(value).map_err(error)?;
            if extraction.facts.len() > 100 {
                return Err(error("抽取事实数量异常"));
            }
            for mut fact in extraction.facts {
                if !matches!(fact.fact_type.as_str(), "world" | "experience")
                    || fact.text.trim().is_empty()
                    || fact.text.len() > 8000
                    || fact.source_message_ids.is_empty()
                    || fact
                        .source_message_ids
                        .iter()
                        .any(|i| !chunk.iter().any(|s| &s.id == i))
                {
                    return Err(error("抽取类型、正文或来源无效"));
                }
                for date in [&mut fact.occurred_start, &mut fact.occurred_end] {
                    if let Some(value) = date {
                        *value = DateTime::parse_from_rfc3339(value)
                            .map_err(error)?
                            .with_timezone(&Utc)
                            .to_rfc3339();
                    }
                }
                if fact.occurred_start.is_some() != fact.occurred_end.is_some()
                    || match (&fact.occurred_start, &fact.occurred_end) {
                        (Some(start), Some(end)) => {
                            DateTime::parse_from_rfc3339(start).map_err(error)?
                                > DateTime::parse_from_rfc3339(end).map_err(error)?
                        }
                        _ => false,
                    }
                {
                    return Err(error("事实发生时间区间无效"));
                }
                if fact.fact_type == "world"
                    && !fact
                        .source_message_ids
                        .iter()
                        .any(|i| chunk.iter().any(|s| &s.id == i && s.role == "user"))
                {
                    continue;
                }
                if fact.fact_type == "experience"
                    && !fact.source_message_ids.iter().any(|i| {
                        chunk
                            .iter()
                            .any(|s| &s.id == i && s.role == "tool" && !s.recalled)
                    })
                {
                    continue;
                }
                let mentioned = date_extreme(
                    chunk
                        .iter()
                        .filter(|s| fact.source_message_ids.contains(&s.id))
                        .map(|s| s.mentioned_at.as_str()),
                    true,
                )?
                .unwrap();
                let timestamp = now();
                facts.push(Unit {
                    id: id("fact"),
                    text: fact.text,
                    fact_type: fact.fact_type,
                    mentioned_at: mentioned,
                    occurred_start: fact.occurred_start,
                    occurred_end: fact.occurred_end,
                    created_at: timestamp.clone(),
                    updated_at: timestamp,
                    entities: fact.entities,
                    source_message_ids: fact.source_message_ids,
                    source_memory_ids: vec![],
                    document_id: Some(document.into()),
                    proof_count: 0,
                    metadata: metadata.clone(),
                    consolidated: false,
                });
            }
        }
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction().map_err(error)?;
        let live: i64 = tx
            .query_row(
                "SELECT count(*) FROM documents WHERE id=? AND visible=1",
                [document],
                |r| r.get(0),
            )
            .map_err(error)?;
        if live > 0 {
            for fact in facts {
                let suppressed = fact.source_message_ids.iter().any(|i| {
                    tx.query_row(
                        "SELECT count(*) FROM suppressed_sources WHERE id=?",
                        [i],
                        |r| r.get::<_, i64>(0),
                    )
                    .unwrap_or(1)
                        > 0
                });
                if !suppressed {
                    put(&tx, &fact)?;
                    kept += 1;
                }
            }
            if kept > 0 {
                tx.execute("INSERT INTO jobs(id,document,session,kind,state,payload) VALUES(?,?,?,'consolidate','queued',?)",params![id("job"),document,session,json!({"model":payload["consolidation"]}).to_string()]).map_err(error)?;
            }
        }
        tx.execute("UPDATE jobs SET state='done',error=NULL,input_tokens=?,output_tokens=? WHERE id=? AND state='running'",params![used.input,used.output,job]).map_err(error)?;
        tx.commit().map_err(error)?;
        drop(db);
        self.emit(
            job, document, session, "extract", "done", None, kept, 0, 0, 0,
        );
        Ok(())
    }
    fn consolidate(
        &self,
        processor: &Arc<dyn Processor>,
        job: &str,
        document: &str,
        session: &str,
        payload: &Value,
    ) -> Result<()> {
        let facts = {
            let db = self.db.lock().unwrap();
            let all = units(&db)?;
            let facts = all
                .iter()
                .filter(|u| {
                    u.fact_type != "observation"
                        && !u.consolidated
                        && u.document_id.as_deref() == Some(document)
                })
                .cloned()
                .collect::<Vec<_>>();
            facts
        };
        let mut used = Usage::default();
        let mut created = 0;
        let mut updated = 0;
        let mut deleted = 0;
        for chunk in facts.chunks(12) {
            let existing = {
                let db = self.db.lock().unwrap();
                let mut candidates = Vec::new();
                let mut seen = HashSet::new();
                for fact in chunk {
                    for obs in search(
                        &db,
                        &format!("{} {}", fact.text, fact.entities.join(" ")),
                        &["observation"],
                        false,
                        5,
                        None,
                        None,
                    )? {
                        if seen.insert(obs.id.clone()) {
                            candidates.push(obs);
                        }
                    }
                }
                candidates.truncate(30);
                candidates
            };
            let (value, usage) = processor.call(
                &payload["model"],
                prompts::CONSOLIDATE,
                &json!({"NEW FACTS":chunk,"EXISTING OBSERVATIONS":existing}),
            )?;
            used.add(usage);
            let actions: Actions = serde_json::from_value(value).map_err(error)?;
            let mut touched = HashSet::new();
            for update in &actions.updates {
                if !touched.insert(update.observation_id.clone())
                    || !existing.iter().any(|u| u.id == update.observation_id)
                {
                    return Err(error("沉淀更新目标无效或重复"));
                }
            }
            for delete in &actions.deletes {
                if delete.reason.trim().is_empty()
                    || !touched.insert(delete.observation_id.clone())
                    || !existing.iter().any(|u| u.id == delete.observation_id)
                {
                    return Err(error("沉淀删除目标无效或重复"));
                }
            }
            let validate = |text: &str, ids: &[String], reason: &str| -> Result<()> {
                if text.trim().is_empty()
                    || text.len() > 8000
                    || reason.trim().is_empty()
                    || ids.is_empty()
                    || ids.iter().any(|i| !chunk.iter().any(|u| &u.id == i))
                {
                    Err(error("沉淀正文或证据来源无效"))
                } else {
                    Ok(())
                }
            };
            for c in &actions.creates {
                validate(&c.text, &c.source_fact_ids, &c.reason)?;
            }
            for u in &actions.updates {
                validate(&u.text, &u.source_fact_ids, &u.reason)?;
            }
            let mut db = self.db.lock().unwrap();
            let tx = db.transaction().map_err(error)?;
            let live = units(&tx)?;
            if chunk.iter().any(|f| {
                !live
                    .iter()
                    .any(|u| u.id == f.id && u.updated_at == f.updated_at)
            }) {
                return Err(error("来源在沉淀期间变化，请重试"));
            }
            for target in &touched {
                let old = existing.iter().find(|u| &u.id == target).unwrap();
                if !live
                    .iter()
                    .any(|u| u.id == old.id && u.updated_at == old.updated_at)
                {
                    return Err(error("观察已变化，请重试"));
                }
            }
            for d in actions.deletes {
                remove(&tx, &d.observation_id)?;
                deleted += 1;
            }
            for u in actions.updates {
                let mut obs = live
                    .iter()
                    .find(|o| o.id == u.observation_id)
                    .unwrap()
                    .clone();
                obs.text = u.text;
                obs.source_memory_ids.extend(u.source_fact_ids);
                refresh_observation(&mut obs, &live)?;
                put(&tx, &obs)?;
                updated += 1;
            }
            for c in actions.creates {
                let timestamp = now();
                let mut obs = Unit {
                    id: id("obs"),
                    text: c.text,
                    fact_type: "observation".into(),
                    mentioned_at: timestamp.clone(),
                    occurred_start: None,
                    occurred_end: None,
                    created_at: timestamp.clone(),
                    updated_at: timestamp,
                    entities: vec![],
                    source_message_ids: vec![],
                    source_memory_ids: c.source_fact_ids,
                    document_id: None,
                    proof_count: 0,
                    metadata: json!({}),
                    consolidated: true,
                };
                refresh_observation(&mut obs, &live)?;
                let current = units(&tx)?;
                if let Some(old) = current
                    .iter()
                    .find(|u| u.fact_type == "observation" && u.text == obs.text)
                {
                    let mut old = old.clone();
                    old.source_memory_ids.extend(obs.source_memory_ids);
                    refresh_observation(&mut old, &current)?;
                    put(&tx, &old)?;
                    updated += 1;
                } else {
                    put(&tx, &obs)?;
                    created += 1;
                }
            }
            for f in chunk {
                let mut f = f.clone();
                f.consolidated = true;
                put(&tx, &f)?;
            }
            tx.commit().map_err(error)?;
        }
        self.db.lock().unwrap().execute("UPDATE jobs SET state='done',error=NULL,input_tokens=?,output_tokens=? WHERE id=? AND state='running'",params![used.input,used.output,job]).map_err(error)?;
        self.emit(
            job,
            document,
            session,
            "consolidate",
            "done",
            None,
            0,
            created,
            updated,
            deleted,
        );
        Ok(())
    }
    pub fn open(path: &Path, enabled: bool) -> Result<(Arc<Self>, Receiver<MemoryJobEvent>)> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(error)?;
        }
        let db = Connection::open(path).map_err(error)?;
        db.busy_timeout(Duration::from_secs(5)).map_err(error)?;
        db.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;",
        )
        .map_err(error)?;
        let version: i64 = db
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .map_err(error)?;
        if version > 1 {
            return Err(error("记忆数据库版本过新"));
        }
        db.execute_batch("BEGIN;
          CREATE TABLE IF NOT EXISTS documents(id TEXT PRIMARY KEY, session TEXT NOT NULL, hash TEXT NOT NULL, sources TEXT NOT NULL, metadata TEXT NOT NULL, mentioned TEXT NOT NULL, visible INTEGER NOT NULL DEFAULT 1);
          CREATE TABLE IF NOT EXISTS units(id TEXT PRIMARY KEY, kind TEXT NOT NULL, data TEXT NOT NULL, consolidated INTEGER NOT NULL DEFAULT 0);
          CREATE VIRTUAL TABLE IF NOT EXISTS unit_search USING fts5(id UNINDEXED, terms, tokenize='unicode61');
          CREATE TABLE IF NOT EXISTS jobs(id TEXT PRIMARY KEY, document TEXT NOT NULL, session TEXT NOT NULL, kind TEXT NOT NULL, state TEXT NOT NULL, payload TEXT NOT NULL, attempts INTEGER NOT NULL DEFAULT 0, error TEXT, input_tokens INTEGER NOT NULL DEFAULT 0, output_tokens INTEGER NOT NULL DEFAULT 0);
          CREATE TABLE IF NOT EXISTS suppressed_sources(id TEXT PRIMARY KEY);
          PRAGMA user_version=1; COMMIT;
          UPDATE jobs SET state='queued' WHERE state='running';").map_err(error)?;
        let (events, receiver) = mpsc::channel();
        let (wake, wake_rx) = mpsc::channel();
        let core = Arc::new(Self {
            db: Mutex::new(db),
            enabled: AtomicBool::new(enabled),
            generation: std::sync::atomic::AtomicU64::new(0),
            processor: RwLock::new(None),
            events,
            wake,
        });
        let weak = Arc::downgrade(&core);
        std::thread::spawn(move || loop {
            let Some(core) = weak.upgrade() else {
                break;
            };
            let progressed = core.run_one();
            drop(core);
            if !progressed {
                let _ = wake_rx.recv_timeout(Duration::from_millis(300));
            }
        });
        Ok((core, receiver))
    }
    pub fn set_enabled(&self, enabled: bool) {
        if !enabled {
            self.generation.fetch_add(1, Ordering::SeqCst);
        }
        self.enabled.store(enabled, Ordering::SeqCst);
        let _ = self.wake.send(());
    }
    pub fn enabled(&self) -> bool {
        self.enabled.load(Ordering::SeqCst)
    }
    pub fn set_processor(&self, processor: Arc<dyn Processor>) {
        *self.processor.write().unwrap() = Some(processor);
        let _ = self.wake.send(());
    }
    pub fn scope(
        self: &Arc<Self>,
        session: String,
        workspace: Option<String>,
        config: MemoryConfig,
        extraction: Value,
        consolidation: Value,
    ) -> Arc<Scope> {
        Arc::new(Scope {
            core: self.clone(),
            session,
            workspace,
            config,
            extraction,
            consolidation,
            forget_turn: Arc::new(AtomicBool::new(false)),
            generation: self.generation.load(Ordering::SeqCst),
        })
    }
    pub fn retry_failed(&self) -> Result<()> {
        self.db
            .lock()
            .unwrap()
            .execute(
                "UPDATE jobs SET state='queued',error=NULL WHERE state='failed'",
                [],
            )
            .map_err(error)?;
        let _ = self.wake.send(());
        Ok(())
    }
    pub fn browse(&self, query: &str, offset: usize) -> Result<Vec<Unit>> {
        self.browse_view(query, offset, BrowseView::Memory)
    }
    pub fn browse_view(&self, query: &str, offset: usize, view: BrowseView) -> Result<Vec<Unit>> {
        let db = self.db.lock().unwrap();
        if !query.trim().is_empty() {
            return Ok(search_in_view(
                &db,
                query,
                &["world", "experience", "observation"],
                false,
                200,
                None,
                None,
                Some(view),
            )?
            .into_iter()
            .skip(offset)
            .take(25)
            .collect());
        }
        let sql = format!("SELECT u.data FROM units u WHERE {} ORDER BY julianday(json_extract(u.data,'$.mentioned_at')) DESC, u.rowid DESC LIMIT 25 OFFSET ?", view.predicate());
        let mut stmt = db.prepare(&sql).map_err(error)?;
        let data = stmt
            .query_map([offset as i64], |r| r.get::<_, String>(0))
            .map_err(error)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(error)?;
        data.into_iter()
            .map(|s| serde_json::from_str(&s).map_err(error))
            .collect()
    }
    pub fn status(&self) -> Result<Value> {
        let db = self.db.lock().unwrap();
        let counts = |sql: &str| db.query_row(sql, [], |r| r.get::<_, i64>(0)).map_err(error);
        let last_error: Option<String> = db
            .query_row(
                "SELECT error FROM jobs WHERE state='failed' ORDER BY rowid DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .ok();
        Ok(
            json!({"enabled":self.enabled(),"documents":counts("SELECT count(*) FROM documents WHERE visible=1")?,"facts":counts("SELECT count(*) FROM units WHERE kind!='observation'")?,"observations":counts("SELECT count(*) FROM units WHERE kind='observation'")?,"queued":counts("SELECT count(*) FROM jobs WHERE state='queued'")?,"running":counts("SELECT count(*) FROM jobs WHERE state='running'")?,"failed":counts("SELECT count(*) FROM jobs WHERE state='failed'")?,"input_tokens":counts("SELECT coalesce(sum(input_tokens),0) FROM jobs")?,"output_tokens":counts("SELECT coalesce(sum(output_tokens),0) FROM jobs")?,"last_error":last_error}),
        )
    }
    fn run_one(&self) -> bool {
        if !self.enabled() {
            return false;
        }
        let Some(processor) = self.processor.read().unwrap().clone() else {
            return false;
        };
        let job = {
            let db = self.db.lock().unwrap();
            if !self.enabled() {
                return false;
            }
            let job = db.query_row("SELECT id,document,session,kind,payload FROM jobs WHERE state='queued' ORDER BY rowid LIMIT 1", [], |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?))).ok();
            if let Some((ref job_id, ..)) = job {
                if db
                    .execute(
                        "UPDATE jobs SET state='running',attempts=attempts+1 WHERE id=?",
                        [job_id],
                    )
                    .is_err()
                {
                    return false;
                }
            }
            job
        };
        let Some((job_id, document, session, kind, payload)) = job else {
            return false;
        };
        self.emit(
            &job_id, &document, &session, &kind, "running", None, 0, 0, 0, 0,
        );
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let payload: Value = serde_json::from_str(&payload).map_err(error)?;
            if kind == "extract" {
                self.extract(&processor, &job_id, &document, &session, &payload)
            } else {
                self.consolidate(&processor, &job_id, &document, &session, &payload)
            }
        }))
        .unwrap_or_else(|_| Err(error("记忆任务异常，原文已保留，可重试")));
        if let Err(err) = result {
            let _ = self.db.lock().unwrap().execute(
                "UPDATE jobs SET state='failed',error=? WHERE id=? AND state='running'",
                params![err.message, job_id],
            );
            self.emit(
                &job_id,
                &document,
                &session,
                &kind,
                "failed",
                Some(err.message),
                0,
                0,
                0,
                0,
            );
        }
        true
    }
    #[allow(clippy::too_many_arguments)]
    fn emit(
        &self,
        job: &str,
        document: &str,
        session: &str,
        kind: &str,
        state: &str,
        error: Option<String>,
        kept: u32,
        created: u32,
        updated: u32,
        deleted: u32,
    ) {
        let _ = self.events.send(MemoryJobEvent {
            session_id: session.into(),
            job_id: job.into(),
            document_id: document.into(),
            kind: kind.into(),
            state: state.into(),
            error,
            kept,
            created,
            updated,
            deleted,
        });
    }
}

fn cjk(c: char) -> bool {
    matches!(c as u32, 0x3400..=0x9fff | 0xf900..=0xfaff)
}
pub fn terms(text: &str) -> BTreeSet<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = BTreeSet::new();
    let mut latin = String::new();
    for (i, c) in chars.iter().copied().enumerate() {
        if cjk(c) {
            if !latin.is_empty() {
                out.insert(std::mem::take(&mut latin));
            }
            out.insert(format!("z{:x}", c as u32));
            if let Some(n) = chars.get(i + 1).filter(|n| cjk(**n)) {
                out.insert(format!("z{:x}z{:x}", c as u32, *n as u32));
            }
        } else if c.is_alphanumeric() || c == '_' {
            latin.extend(c.to_lowercase());
        } else if !latin.is_empty() {
            out.insert(std::mem::take(&mut latin));
        }
    }
    if !latin.is_empty() {
        out.insert(latin);
    }
    out
}
fn put(db: &Connection, unit: &Unit) -> Result<()> {
    db.execute("INSERT INTO units(id,kind,data,consolidated) VALUES(?,?,?,?) ON CONFLICT(id) DO UPDATE SET data=excluded.data,consolidated=excluded.consolidated", params![unit.id, unit.fact_type, serde_json::to_string(unit).map_err(error)?,unit.consolidated]).map_err(error)?;
    db.execute("DELETE FROM unit_search WHERE id=?", [&unit.id])
        .map_err(error)?;
    let indexed = terms(&format!(
        "{} {} {}",
        unit.text,
        unit.entities.join(" "),
        unit.metadata
            .get("workspace")
            .and_then(Value::as_str)
            .unwrap_or_default()
    ))
    .into_iter()
    .collect::<Vec<_>>()
    .join(" ");
    db.execute(
        "INSERT INTO unit_search(id,terms) VALUES(?,?)",
        params![unit.id, indexed],
    )
    .map_err(error)?;
    Ok(())
}
fn remove(db: &Connection, unit: &str) -> Result<()> {
    db.execute("DELETE FROM units WHERE id=?", [unit])
        .map_err(error)?;
    db.execute("DELETE FROM unit_search WHERE id=?", [unit])
        .map_err(error)?;
    Ok(())
}
fn units(db: &Connection) -> Result<Vec<Unit>> {
    let mut stmt = db.prepare("SELECT data FROM units").map_err(error)?;
    let data = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(error)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(error)?;
    data.into_iter()
        .map(|s| serde_json::from_str(&s).map_err(error))
        .collect()
}
fn search(
    db: &Connection,
    query: &str,
    kinds: &[&str],
    pending_only: bool,
    limit: usize,
    start: Option<&str>,
    end: Option<&str>,
) -> Result<Vec<Unit>> {
    search_in_view(db, query, kinds, pending_only, limit, start, end, None)
}
#[allow(clippy::too_many_arguments)]
fn search_in_view(
    db: &Connection,
    query: &str,
    kinds: &[&str],
    pending_only: bool,
    limit: usize,
    start: Option<&str>,
    end: Option<&str>,
    view: Option<BrowseView>,
) -> Result<Vec<Unit>> {
    if query.len() > 4000 {
        return Err(error("记忆查询过长"));
    }
    let query_terms = terms(query);
    if query_terms.is_empty() {
        return Ok(vec![]);
    }
    let expr = query_terms
        .iter()
        .map(|s| format!("\"{s}\""))
        .collect::<Vec<_>>()
        .join(" OR ");
    let sql = format!("SELECT u.data FROM unit_search s JOIN units u ON u.id=s.id WHERE unit_search MATCH ? AND {} ORDER BY bm25(unit_search) LIMIT 200", view.map(BrowseView::predicate).unwrap_or("1"));
    let mut stmt = db.prepare(&sql).map_err(error)?;
    let data = stmt
        .query_map([expr], |r| r.get::<_, String>(0))
        .map_err(error)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(error)?;
    let mut matched = Vec::new();
    for data in data {
        let unit: Unit = serde_json::from_str(&data).map_err(error)?;
        if !kinds.contains(&unit.fact_type.as_str()) || (pending_only && unit.consolidated) {
            continue;
        }
        if let Some(start) = start {
            if DateTime::parse_from_rfc3339(
                unit.occurred_end.as_deref().unwrap_or(&unit.mentioned_at),
            )
            .map_err(error)?
                < DateTime::parse_from_rfc3339(start).map_err(error)?
            {
                continue;
            }
        }
        if let Some(end) = end {
            if DateTime::parse_from_rfc3339(
                unit.occurred_start.as_deref().unwrap_or(&unit.mentioned_at),
            )
            .map_err(error)?
                > DateTime::parse_from_rfc3339(end).map_err(error)?
            {
                continue;
            }
        }
        let own = terms(&unit.text);
        // Single Chinese characters alone are weak matches unless the query itself is one character.
        let strong = query_terms
            .intersection(&own)
            .any(|t| !t.starts_with('z') || t.matches('z').count() > 1)
            || query.chars().count() == 1;
        let entity = unit.entities.iter().any(|entity| {
            entity.chars().count() >= 2 && query.to_lowercase().contains(&entity.to_lowercase())
        });
        if strong || entity {
            matched.push(unit);
        }
        if matched.len() >= limit {
            break;
        }
    }
    Ok(matched)
}

/// Conservative Unicode token allowance. Includes JSON overhead and never cuts JSON bytes.
pub fn estimate_tokens(text: &str) -> usize {
    // A byte upper bound works for arbitrary text and model vocabularies without an embedding/tokenizer dependency.
    text.len()
}
pub fn bounded(mut value: Value, key: &str, max_tokens: usize) -> Value {
    let max_tokens = max_tokens.min(5000);
    value["truncated"] = false.into();
    while estimate_tokens(&value.to_string()) > max_tokens {
        value["truncated"] = true.into();
        let Some(items) = value[key].as_array_mut() else {
            break;
        };
        let Some(last) = items.last_mut() else {
            break;
        };
        if let Some(text) = last.get_mut("text") {
            let raw = text.as_str().unwrap_or_default();
            let count = raw.chars().count();
            if count > 40 {
                *text = raw
                    .chars()
                    .take(count.saturating_sub((count / 4).max(1)))
                    .collect::<String>()
                    .into();
                continue;
            }
        }
        items.pop();
    }
    value
}

impl Scope {
    pub fn forget(&self, query: &str, ids: &[String]) -> Result<Value> {
        if !self.enabled() {
            return Err(error("记忆已关闭"));
        }
        if query.trim().is_empty() || ids.is_empty() || ids.len() > 20 {
            return Err(error("遗忘需要明确请求和 1–20 个已定位记忆 ID"));
        }
        let mut db = self.core.db.lock().unwrap();
        let tx = db.transaction().map_err(error)?;
        let all = units(&tx)?;
        let mut selected = HashSet::new();
        for target in ids {
            let unit = all
                .iter()
                .find(|u| &u.id == target)
                .ok_or_else(|| error("目标不存在或已遗忘"))?;
            if unit.fact_type == "observation" {
                selected.extend(unit.source_memory_ids.iter().cloned());
            } else {
                selected.insert(unit.id.clone());
            }
        }
        let suppressed: HashSet<String> = all
            .iter()
            .filter(|u| selected.contains(&u.id))
            .flat_map(|u| u.source_message_ids.iter().cloned())
            .collect();
        // Source messages are the minimum retained evidence unit. Other facts relying on the same source are invalidated too.
        selected.extend(
            all.iter()
                .filter(|u| u.source_message_ids.iter().any(|s| suppressed.contains(s)))
                .map(|u| u.id.clone()),
        );
        let mut affected = Vec::new();
        let mut rebuild = HashSet::new();
        for obs in all.iter().filter(|u| u.fact_type == "observation") {
            if obs.source_memory_ids.iter().any(|i| selected.contains(i)) {
                affected.push(obs.id.clone());
                rebuild.extend(
                    obs.source_memory_ids
                        .iter()
                        .filter(|i| !selected.contains(*i))
                        .cloned(),
                );
            }
        }
        for source in &suppressed {
            tx.execute(
                "INSERT OR IGNORE INTO suppressed_sources(id) VALUES(?)",
                [source],
            )
            .map_err(error)?;
        }
        let mut stmt = tx
            .prepare("SELECT id,sources FROM documents WHERE visible=1")
            .map_err(error)?;
        let docs = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .map_err(error)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(error)?;
        drop(stmt);
        for (doc, data) in docs {
            let mut sources: Vec<Source> = serde_json::from_str(&data).map_err(error)?;
            if sources.iter().any(|s| suppressed.contains(&s.id)) {
                sources.retain(|s| !suppressed.contains(&s.id));
                tx.execute(
                    "UPDATE documents SET sources=?,visible=? WHERE id=?",
                    params![
                        serde_json::to_string(&sources).map_err(error)?,
                        !sources.is_empty(),
                        doc
                    ],
                )
                .map_err(error)?;
            }
        }
        for target in selected.iter().chain(affected.iter()) {
            remove(&tx, target)?;
        }
        for target in rebuild {
            if let Some(f) = all.iter().find(|u| u.id == target) {
                let mut f = f.clone();
                f.consolidated = false;
                f.updated_at = now();
                put(&tx, &f)?;
                if let Some(doc) = &f.document_id {
                    tx.execute("INSERT INTO jobs(id,document,session,kind,state,payload) VALUES(?,?,?,'consolidate','queued',?)",params![id("job"),doc,self.session,json!({"model":self.consolidation}).to_string()]).map_err(error)?;
                }
            }
        }
        tx.commit().map_err(error)?;
        self.forget_turn.store(true, Ordering::SeqCst);
        Ok(
            json!({"state":"done","deleted_facts":selected.len(),"deleted_observations":affected.len(),"suppressed_sources":suppressed.len(),"scope":"相关事实及其来源消息；聊天记录保留"}),
        )
    }
    pub fn register(self: &Arc<Self>, registry: &mut ToolRegistry) -> Result<()> {
        tools::register(self.clone(), registry)
    }
    pub fn query(
        &self,
        query: &str,
        kinds: &[&str],
        limit: usize,
        tokens: usize,
        start: Option<&str>,
        end: Option<&str>,
    ) -> Result<Value> {
        if !self.enabled() {
            return Err(error("记忆已关闭"));
        }
        let db = self.core.db.lock().unwrap();
        let mut items = search(&db, query, kinds, false, limit.clamp(1, 5), start, end)?;
        let stale = db
            .query_row(
                "SELECT count(*) FROM units WHERE kind!='observation' AND consolidated=0",
                [],
                |r| r.get::<_, i64>(0),
            )
            .map_err(error)?
            > 0;
        let serialized = items
            .drain(..)
            .map(|u| {
                let mut v = serde_json::to_value(u).unwrap();
                v["is_stale"] = stale.into();
                v
            })
            .collect::<Vec<_>>();
        Ok(bounded(
            json!({"items":serialized,"is_stale":stale}),
            "items",
            tokens,
        ))
    }
    pub fn expand(&self, ids: &[String], depth: &str, tokens: usize) -> Result<Value> {
        if !self.enabled() {
            return Err(error("记忆已关闭"));
        }
        if ids.is_empty() || ids.len() > 5 || !matches!(depth, "chunk" | "document") {
            return Err(error("expand 需要 1–5 个事实 ID 和 chunk/document 深度"));
        }
        let db = self.core.db.lock().unwrap();
        let all = units(&db)?;
        let mut out = Vec::new();
        for id in ids {
            let unit = all
                .iter()
                .find(|u| &u.id == id)
                .ok_or_else(|| error("来源不存在或已遗忘"))?;
            let doc = unit
                .document_id
                .as_ref()
                .ok_or_else(|| error("observation 请先选择 source_memory_ids 中的事实"))?;
            let data: String = db
                .query_row(
                    "SELECT sources FROM documents WHERE id=? AND visible=1",
                    [doc],
                    |r| r.get(0),
                )
                .map_err(|_| error("来源不存在或已遗忘"))?;
            let sources: Vec<Source> = serde_json::from_str(&data).map_err(error)?;
            let sources: Vec<_> = sources
                .into_iter()
                .filter(|s| depth == "document" || unit.source_message_ids.contains(&s.id))
                .collect();
            out.push(json!({"id":id,"document_id":doc,"text":serde_json::to_string(&sources).map_err(error)?}));
        }
        Ok(bounded(json!({"items":out}), "items", tokens.min(5000)))
    }
}

fn refresh_observation(obs: &mut Unit, all: &[Unit]) -> Result<()> {
    obs.source_memory_ids.sort();
    obs.source_memory_ids.dedup();
    let sources = obs
        .source_memory_ids
        .iter()
        .map(|i| {
            all.iter()
                .find(|u| &u.id == i)
                .ok_or_else(|| error("观察来源不存在"))
        })
        .collect::<Result<Vec<_>>>()?;
    obs.proof_count = sources.len() as u32;
    obs.updated_at = now();
    obs.mentioned_at =
        date_extreme(sources.iter().map(|u| u.mentioned_at.as_str()), true)?.unwrap_or_else(now);
    obs.occurred_start = date_extreme(
        sources.iter().filter_map(|u| u.occurred_start.as_deref()),
        false,
    )?;
    obs.occurred_end = date_extreme(
        sources.iter().filter_map(|u| u.occurred_end.as_deref()),
        true,
    )?;
    obs.entities = sources
        .iter()
        .flat_map(|u| u.entities.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    Ok(())
}

impl Memory for Scope {
    fn enabled(&self) -> bool {
        self.config.enabled
            && self.core.enabled()
            && self.generation == self.core.generation.load(Ordering::SeqCst)
    }
    fn policy(&self) -> &str {
        prompts::POLICY
    }
    fn recall(&self, query: &str) -> Vec<RecallItem> {
        self.recall_checked(query).unwrap_or_default()
    }
    fn recall_checked(&self, query: &str) -> Result<Vec<RecallItem>> {
        if !self.enabled() {
            return Ok(vec![]);
        }
        let db = self.core.db.lock().unwrap();
        let limit = self.config.auto_items.min(5);
        let mut observations = search(&db, query, &["observation"], false, limit, None, None)?;
        let pending = search(
            &db,
            query,
            &["world", "experience"],
            true,
            limit,
            None,
            None,
        )?;
        let stale = !pending.is_empty();
        let mut items = pending;
        observations.retain(|o| {
            !o.source_memory_ids
                .iter()
                .any(|i| items.iter().any(|f| &f.id == i))
        });
        items.append(&mut observations);
        items.truncate(limit);
        let serialized = items.iter().map(|u| json!({"id":u.id,"fact_type":u.fact_type,"text":u.text,"proof_count":u.proof_count,"mentioned_at":u.mentioned_at,"occurred_start":u.occurred_start,"occurred_end":u.occurred_end,"source_memory_ids":u.source_memory_ids,"is_stale":u.fact_type=="observation" && stale})).collect::<Vec<_>>();
        let value = bounded(
            json!({"items":serialized}),
            "items",
            self.config.auto_tokens.min(1000),
        );
        serde_json::from_value(value["items"].clone()).map_err(error)
    }
    fn retain(&self, document_id: &str, messages: &[Message]) -> Result<RetainReceipt> {
        if !self.enabled() {
            return Err(error("记忆已关闭，未提交新原文"));
        }
        if self.forget_turn.load(Ordering::SeqCst) {
            return Ok(RetainReceipt {
                kept: 0,
                dropped: messages.len() as u32,
                pending: false,
            });
        }
        let start: usize = document_id
            .rsplit_once('@')
            .and_then(|(_, range)| range.split_once('-'))
            .and_then(|(s, _)| s.parse().ok())
            .ok_or_else(|| error("记忆来源范围无效"))?;
        let timestamp = now();
        let mut sources: Vec<Source> = messages
            .iter()
            .enumerate()
            .filter_map(|(i, m)| {
                let (role, text, is_error) = match m {
                    Message::User { text, .. } => ("user", text.clone(), false),
                    Message::Assistant { text, .. } if !text.is_empty() => {
                        ("assistant", text.clone(), false)
                    }
                    Message::ToolResult {
                        name,
                        text,
                        is_error,
                        ..
                    } if !is_memory_tool(name) => ("tool", format!("{name}: {text}"), *is_error),
                    _ => return None,
                };
                Some(Source {
                    id: m
                        .origin()
                        .map(|o| o.id.clone())
                        .unwrap_or_else(|| format!("{}:{}", self.session, start + i)),
                    role: role.into(),
                    text,
                    mentioned_at: m
                        .origin()
                        .map(|o| o.mentioned_at.clone())
                        .unwrap_or_else(|| timestamp.clone()),
                    is_error,
                    recalled: m.origin().is_some_and(|o| o.recalled),
                })
            })
            .collect();
        let hash = content_hash(messages);
        let mut db = self.core.db.lock().unwrap();
        if !self.enabled() {
            return Err(error("记忆已关闭"));
        }
        sources.retain(|s| {
            db.query_row(
                "SELECT count(*) FROM suppressed_sources WHERE id=?",
                [&s.id],
                |r| r.get::<_, i64>(0),
            )
            .unwrap_or(1)
                == 0
        });
        let existing: Option<String> = db
            .query_row(
                "SELECT hash FROM documents WHERE id=?",
                [document_id],
                |r| r.get(0),
            )
            .ok();
        if existing.as_deref() == Some(&hash) {
            return Ok(RetainReceipt {
                kept: 0,
                dropped: 0,
                pending: true,
            });
        }
        if existing.is_some() {
            return Err(error("同一稳定来源内容已变化，拒绝覆盖"));
        }
        let tx = db.transaction().map_err(error)?;
        tx.execute(
            "INSERT INTO documents(id,session,hash,sources,metadata,mentioned) VALUES(?,?,?,?,?,?)",
            params![
                document_id,
                self.session,
                hash,
                serde_json::to_string(&sources).map_err(error)?,
                json!({"session":self.session,"workspace":self.workspace}).to_string(),
                timestamp
            ],
        )
        .map_err(error)?;
        if !sources.is_empty() {
            tx.execute("INSERT INTO jobs(id,document,session,kind,state,payload) VALUES(?,?,?,'extract','queued',?)", params![id("job"),document_id,self.session,json!({"model":self.extraction,"consolidation":self.consolidation}).to_string()]).map_err(error)?;
        }
        tx.commit().map_err(error)?;
        let _ = self.core.wake.send(());
        Ok(RetainReceipt {
            kept: 0,
            dropped: 0,
            pending: !sources.is_empty(),
        })
    }
    fn consolidate_pending(&self) -> Result<ConsolidateReceipt> {
        Ok(ConsolidateReceipt {
            created: 0,
            updated: 0,
        })
    }
}
