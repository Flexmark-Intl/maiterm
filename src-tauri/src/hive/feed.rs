//! The Hive board's pushed feed (docs/hive.md §4.4): each worker's model and context meta here,
//! its diff stat in `diffstat`. Both are recomputed only when the worker's own hooks say something
//! changed (`events::observed`), coalesced per key, and pushed to its Hive's window alone (`emit`
//! would reach every window's board). Nothing runs on a timer: an idle worker costs nothing.

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, LazyLock, OnceLock};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde::Serialize;
use serde_json::{json, Value};
use tauri::AppHandle;

use super::tools::{blocking, Caller};
use super::ROLE_WORKER;
use crate::state::{AppData, AppState};

pub const META_EVENT: &str = "hive-worker-meta";
/// A Stop and the SessionStart after a compaction can come close together; one transcript read covers both.
const META_DEBOUNCE_MS: i64 = 2_000;

/// Coalesces recomputes per key. The first touch arms the key and sets its deadline; touches
/// before it add nothing, so a busy worker is recomputed at most once a window and never starved
/// the way a trailing debounce would starve it. A key runs one compute at a time: a deadline that
/// passes during one queues exactly one rerun after it.
pub(crate) struct Debounce {
    window_ms: i64,
    slots: HashMap<String, Slot>,
}

#[derive(Default)]
struct Slot {
    due_at: Option<i64>,
    running: bool,
    rerun: bool,
}

impl Debounce {
    pub fn new(window_ms: i64) -> Self {
        Self { window_ms, slots: HashMap::new() }
    }

    /// A touch: true when it armed the key, so the caller starts the key's one waiter.
    pub fn arm(&mut self, key: &str, now: i64) -> bool {
        let slot = self.slots.entry(key.to_string()).or_default();
        if slot.due_at.is_some() {
            return false;
        }
        slot.due_at = Some(now + self.window_ms);
        true
    }

    /// How long the waiter still sleeps; None once it is due, or the key was forgotten.
    pub fn wait(&self, key: &str, now: i64) -> Option<i64> {
        self.slots.get(key)?.due_at.filter(|t| *t > now).map(|t| t - now)
    }

    /// The waiter woke: true to compute now. A key mid-compute takes it as its one rerun.
    pub fn due(&mut self, key: &str, now: i64) -> bool {
        let Some(slot) = self.slots.get_mut(key) else { return false };
        if !slot.due_at.is_some_and(|t| t <= now) {
            return false;
        }
        slot.due_at = None;
        if slot.running {
            slot.rerun = true;
            return false;
        }
        slot.running = true;
        true
    }

    /// A compute ended: true to run once more, for a deadline that passed during it.
    pub fn finish(&mut self, key: &str) -> bool {
        let Some(slot) = self.slots.get_mut(key) else { return false };
        if slot.rerun {
            slot.rerun = false;
            return true;
        }
        slot.running = false;
        if slot.due_at.is_none() {
            self.slots.remove(key);
        }
        false
    }

    /// Its waiter, if any, wakes to nothing.
    pub fn forget(&mut self, key: &str) {
        self.slots.remove(key);
    }
}

/// Milliseconds on the monotonic clock tokio's sleep runs on, so a waiter never wakes early.
fn clock_ms() -> i64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_millis() as i64
}

/// Runs `job` for `key` once its debounce is due, and again while touches land during it.
pub(crate) fn schedule<F, Fut>(debounce: &'static Mutex<Debounce>, key: String, job: F)
where
    F: Fn() -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    if !debounce.lock().arm(&key, clock_ms()) {
        return;
    }
    tauri::async_runtime::spawn(async move {
        loop {
            let wait = debounce.lock().wait(&key, clock_ms());
            match wait {
                Some(ms) => tokio::time::sleep(Duration::from_millis(ms as u64)).await,
                None => break,
            }
        }
        if !debounce.lock().due(&key, clock_ms()) {
            return;
        }
        loop {
            job().await;
            if !debounce.lock().finish(&key) {
                break;
            }
        }
    });
}

/// A Hive event for one window: `emit_to`, never `emit`.
pub(crate) fn emit(handle: &AppHandle, label: &str, event: &str, payload: Value) {
    use tauri::Emitter;
    if let Err(e) = handle.emit_to(label, event, payload) {
        log::warn!("hive: {event} not sent to window {label}: {e}");
    }
}

// ── Worker meta ──

static META_DEBOUNCE: LazyLock<Mutex<Debounce>> = LazyLock::new(|| Mutex::new(Debounce::new(META_DEBOUNCE_MS)));
/// The last meta pushed per worker tab: a push goes out only when it differs.
static META_CACHE: LazyLock<Mutex<HashMap<String, Value>>> = LazyLock::new(Default::default);

/// One worker tab's meta as `hive_worker_metas` answers: `getTabMeta`'s shape without `queued`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WorkerMeta {
    pub tab_id: String,
    pub meta: Option<Value>,
}

/// Caches `meta` for `tab_id`; true when it differs from what was there.
fn remember_meta(tab_id: &str, meta: &Value) -> bool {
    let mut cache = META_CACHE.lock();
    if cache.get(tab_id) == Some(meta) {
        return false;
    }
    cache.insert(tab_id.to_string(), meta.clone());
    true
}

pub(crate) fn forget_meta(tab_id: &str) {
    META_CACHE.lock().remove(tab_id);
    META_DEBOUNCE.lock().forget(tab_id);
}

/// A worker's Stop or session start: its model or context may have moved. Recomputed from its
/// transcript after the debounce, off the async runtime, and pushed to its Hive's window if changed.
pub(crate) fn meta_touch(state: &Arc<AppState>, handle: &AppHandle, caller: &Caller, tab_id: &str) {
    let (state, handle, label, ws, tab) =
        (state.clone(), handle.clone(), caller.label.clone(), caller.workspace_id.clone(), tab_id.to_string());
    schedule(&META_DEBOUNCE, tab_id.to_string(), move || {
        let (state, handle, label, ws, tab) = (state.clone(), handle.clone(), label.clone(), ws.clone(), tab.clone());
        async move {
            let t = tab.clone();
            let meta = match blocking(move || Ok(crate::mailink::tab_agent_meta(&state, &t))).await {
                Ok(Some(m)) => m,
                Ok(None) => return,
                Err(e) => {
                    log::warn!("hive: worker tab {tab}: meta not read: {e}");
                    return;
                }
            };
            if remember_meta(&tab, &meta) {
                emit(&handle, &label, META_EVENT, json!({ "workspace_id": ws, "tab_id": tab, "meta": meta }));
            }
        }
    });
}

/// The worker tabs of the Hive in `workspace_id`, in window `label`.
fn worker_tabs(data: &AppData, label: &str, workspace_id: &str) -> Result<Vec<String>, String> {
    let ws = data.window(label).and_then(|w| w.workspaces.iter().find(|w| w.id == workspace_id)).ok_or("Workspace not found")?;
    if ws.hive.is_none() {
        return Err("That workspace isn't a Hive".to_string());
    }
    Ok(ws.panes.iter().flat_map(|p| p.tabs.iter()).filter(|t| t.hive_role.as_deref() == Some(ROLE_WORKER)).map(|t| t.id.clone()).collect())
}

/// Every worker tab's meta for the Hive view's mount: from the cache, and a missing one read once
/// here. After this the view keeps up through `hive-worker-meta`.
#[tauri::command]
pub async fn hive_worker_metas(
    window: tauri::Window,
    state: tauri::State<'_, Arc<AppState>>,
    workspace_id: String,
) -> Result<Vec<WorkerMeta>, String> {
    let tabs = worker_tabs(&state.app_data.read(), window.label(), &workspace_id)?;
    let app = state.inner().clone();
    blocking(move || {
        Ok(tabs
            .into_iter()
            .map(|tab_id| {
                let cached = META_CACHE.lock().get(&tab_id).cloned();
                let meta = cached.or_else(|| {
                    let m = crate::mailink::tab_agent_meta(&app, &tab_id)?;
                    remember_meta(&tab_id, &m);
                    Some(m)
                });
                WorkerMeta { tab_id, meta }
            })
            .collect())
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ten_touches_in_one_window_make_one_compute() {
        let mut d = Debounce::new(5_000);
        let armed: Vec<bool> = (0..10).map(|i| d.arm("k", i * 500)).collect();
        assert_eq!(armed.iter().filter(|a| **a).count(), 1, "one waiter");
        assert!(armed[0]);
        assert_eq!(d.wait("k", 4_500), Some(500), "the first touch set the deadline");
        assert!(!d.due("k", 4_999));
        assert!(d.due("k", 5_000));
        assert!(!d.due("k", 5_000), "taken once");
        assert!(!d.finish("k"), "no touch during it: no rerun");
        assert!(d.slots.is_empty(), "an idle key holds nothing");
        assert!(d.arm("k", 6_000), "a later touch arms again");
    }

    #[test]
    fn a_deadline_during_a_compute_queues_exactly_one_rerun() {
        let mut d = Debounce::new(5_000);
        assert!(d.arm("k", 0) && d.due("k", 5_000));
        // Touches while it computes: one new waiter, whose deadline passes mid-compute.
        assert!(d.arm("k", 5_100));
        assert!(!d.arm("k", 6_000));
        assert!(!d.due("k", 10_100), "never two computes at once");
        assert!(d.finish("k"), "one rerun");
        assert!(!d.finish("k"), "and only one");
        assert_eq!(d.wait("k", 0), None);
    }

    #[test]
    fn a_forgotten_key_wakes_to_nothing() {
        let mut d = Debounce::new(5_000);
        assert!(d.arm("k", 0));
        assert!(d.arm("other", 0));
        d.forget("k");
        assert_eq!(d.wait("k", 1_000), None);
        assert!(!d.due("k", 5_000) && !d.finish("k"));
        assert!(d.due("other", 5_000), "keys are separate");
    }

    #[test]
    fn meta_is_pushed_only_when_it_changes() {
        let tab = format!("tab-{}", uuid::Uuid::new_v4());
        let m = json!({ "runtime": "claude", "model": "Sonnet 5", "contextPct": 18 });
        assert!(remember_meta(&tab, &m));
        assert!(!remember_meta(&tab, &m.clone()));
        assert!(remember_meta(&tab, &json!({ "runtime": "claude", "model": "Sonnet 5", "contextPct": 19 })));
        forget_meta(&tab);
        assert!(remember_meta(&tab, &m), "forgotten: the next one is news");
        forget_meta(&tab);
    }
}
