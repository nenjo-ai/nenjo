//! QuickJS-backed script execution engine.
//!
//! The sandbox is structural: the interpreter receives no bindings other than
//! the injected `ctx` object (`ctx.<namespace>.<tool>()` methods, `ctx.<ns>.list()`,
//! and `ctx.log`). There is no filesystem, network, clock, or process access.
//!
//! Native/JS boundary design: the host exposes a single string-in/string-out
//! async dispatch function plus a static JSON tool catalog; a small JS shim
//! builds the `ctx` namespaces from them. No native closure captures `Ctx`,
//! which keeps every native future `Send` and avoids lifetime gymnastics.
//!
//! Every dispatch from a script goes through the same `Tool::execute` pipeline
//! a direct model call would take, so scripts gain control flow, never
//! privileges.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::Result;
use nenjo_tool_api::{Tool, ToolResult};
use rquickjs::{
    AsyncContext, AsyncRuntime, Function, Promise, Value, function::Async, function::Rest,
};
use serde_json::{Map, Number, json};
use tokio::time::Duration;
use tokio_util::sync::CancellationToken;

/// Hard resource caps for a single script execution.
#[derive(Debug, Clone)]
pub struct ScriptLimits {
    /// Interpreter heap limit.
    pub max_memory_bytes: usize,
    /// Interpreter call-stack limit.
    pub max_stack_bytes: usize,
    /// Wall-clock budget when the call does not specify `timeout_ms`.
    pub default_timeout: Duration,
    /// Upper bound for a caller-provided `timeout_ms`.
    pub max_timeout: Duration,
    /// Cap on the JSON-serialized return value handed back to the model.
    pub max_output_bytes: usize,
    /// Caps on incremental `ctx.log` output.
    pub max_log_lines: usize,
    pub max_log_bytes: usize,
}

impl Default for ScriptLimits {
    fn default() -> Self {
        Self {
            max_memory_bytes: 128 * 1024 * 1024,
            max_stack_bytes: 1024 * 1024,
            default_timeout: Duration::from_secs(30),
            max_timeout: Duration::from_secs(120),
            max_output_bytes: 1024 * 1024,
            max_log_lines: 1_000,
            max_log_bytes: 256 * 1024,
        }
    }
}

/// Tool namespaces exposed to a script.
#[derive(Clone, Default)]
pub struct ScriptNamespaces {
    /// Exposed as `ctx.mcp.*`.
    pub mcp: Vec<Arc<dyn Tool>>,
    /// Exposed as `ctx.runtime.*`.
    pub runtime: Vec<Arc<dyn Tool>>,
    /// Exposed as `ctx.harness.*` metadata (session/project identity).
    pub harness: Option<ScriptHarnessContext>,
}

/// Harness-level identity exposed to `harness`-scoped scripts.
#[derive(Debug, Clone, Default)]
pub struct ScriptHarnessContext {
    pub session_id: Option<uuid::Uuid>,
    pub project_slug: Option<String>,
}

/// Result of a script run. Log lines live in the caller-owned
/// [`LogBuffer`], so async-operation streaming can read them while the
/// script is still running.
#[derive(Debug, Default)]
pub struct ScriptOutcome {
    /// JSON-serializable return value of the script.
    pub value: Option<serde_json::Value>,
    /// Error message when the script threw, aborted, or timed out.
    pub error: Option<String>,
    /// True when the wall-clock budget was exhausted.
    pub timed_out: bool,
}

/// Shared, externally readable log buffer fed by `ctx.log`.
#[derive(Default)]
pub struct LogBuffer {
    lines: Vec<String>,
    total_bytes: usize,
    truncated: bool,
    /// Number of lines already consumed by async-operation streaming.
    published: usize,
}

impl LogBuffer {
    pub(crate) fn push(&mut self, limits: &ScriptLimits, line: String) {
        if self.lines.len() >= limits.max_log_lines
            || self.total_bytes + line.len() > limits.max_log_bytes
        {
            self.truncated = true;
            return;
        }
        self.total_bytes += line.len();
        self.lines.push(line);
    }

    /// Lines not yet consumed by a streamer.
    pub(crate) fn take_new(&mut self) -> Vec<String> {
        let new = self.lines[self.published.min(self.lines.len())..].to_vec();
        self.published = self.lines.len();
        new
    }

    /// All lines plus the truncation marker, marking them published. This is
    /// the terminal read used when a run settles.
    pub(crate) fn drain_all(&mut self) -> Vec<String> {
        self.published = self.lines.len();
        let mut lines = self.lines.clone();
        if self.truncated {
            lines.push("[log truncated: cap reached]".to_string());
        }
        lines
    }

    pub(crate) fn truncated(&self) -> bool {
        self.truncated
    }
}

/// Run a script with the given namespaces and limits.
///
/// `stop` cancels the run: the engine's own deadline thread cancels it at
/// `timeout`, and an external supervisor (async-operation stop) may cancel it
/// earlier. Cancellation trips the QuickJS interrupt handler, so CPU-bound
/// loops terminate even without an await point.
pub async fn run(
    script: &str,
    namespaces: ScriptNamespaces,
    limits: &ScriptLimits,
    timeout: Duration,
    stop: CancellationToken,
    logs: Arc<Mutex<LogBuffer>>,
) -> Result<ScriptOutcome> {
    run_with_input(script, namespaces, limits, timeout, stop, logs, None).await
}

/// Run a script that additionally receives `input` as its `args` parameter
/// (used by package-shipped script tools).
pub async fn run_with_input(
    script: &str,
    namespaces: ScriptNamespaces,
    limits: &ScriptLimits,
    timeout: Duration,
    stop: CancellationToken,
    logs: Arc<Mutex<LogBuffer>>,
    input: Option<serde_json::Value>,
) -> Result<ScriptOutcome> {
    let timeout = timeout.min(limits.max_timeout);

    let runtime = AsyncRuntime::new()?;
    runtime.set_memory_limit(limits.max_memory_bytes).await;
    runtime.set_max_stack_size(limits.max_stack_bytes).await;

    // The interrupt handler is polled by QuickJS during execution. A blocked
    // interpreter never yields to the scheduler, so the deadline must live on
    // a dedicated OS thread rather than a timer on the local executor.
    // `deadline_fired` makes timeout classification exact: an elapsed-time
    // check races the interrupt unwind (the unwind can be observed a hair
    // before the clock crosses the deadline), which would misreport a
    // deadline kill as a bare interpreter exception.
    let deadline_fired = Arc::new(AtomicBool::new(false));
    let deadline_flag = Arc::clone(&deadline_fired);
    let deadline_stop = stop.clone();
    std::thread::spawn(move || {
        std::thread::sleep(timeout);
        deadline_flag.store(true, Ordering::Release);
        deadline_stop.cancel();
    });
    let interrupt_stop = stop.clone();
    runtime
        .set_interrupt_handler(Some(Box::new(move || interrupt_stop.is_cancelled())))
        .await;

    let ctx = AsyncContext::full(&runtime).await?;

    // Dispatch registry: JS refers to tools by name; native side resolves.
    let mut by_name: HashMap<String, Arc<dyn Tool>> = HashMap::new();
    for tool in namespaces.mcp.iter().chain(namespaces.runtime.iter()) {
        by_name.insert(tool.name().to_string(), tool.clone());
    }
    let catalog = json!({
        "mcp": namespaces.mcp.iter().map(tool_catalog_entry).collect::<Vec<_>>(),
        "runtime": namespaces.runtime.iter().map(tool_catalog_entry).collect::<Vec<_>>(),
    })
    .to_string();
    let harness = namespaces
        .harness
        .map(|context| {
            json!({
                "sessionId": context.session_id.map(|id| id.to_string()),
                "project": context.project_slug,
            })
        })
        .unwrap_or(serde_json::Value::Null)
        .to_string();

    let install_logs = logs.clone();
    let install_limits = limits.clone();
    // JSON is a valid JS expression, so the input can be inlined safely; the
    // script receives it as the `args` parameter of its entry function.
    let args_literal = input
        .map(|value| serde_json::to_string(&value).unwrap_or_else(|_| "undefined".to_string()))
        .unwrap_or_else(|| "undefined".to_string());
    let body = rquickjs::async_with!(ctx => |ctx| {
        install_context(&ctx, Arc::new(by_name), &catalog, &harness, &install_logs, &install_limits)?;
        let wrapper = format!(
            "globalThis.__nenjo_script_result = (async function(args) {{\n{script}\n}})({args_literal});"
        );
        let returned: Value = ctx.eval(wrapper.as_str())?;
        let promise = Promise::from_value(returned)?;
        match promise.into_future::<Value>().await {
            Ok(value) => Ok(js_to_json(&value)),
            Err(err) => {
                let message = exception_message(&ctx, &err);
                Err(anyhow::anyhow!(message))
            }
        }
    });

    let result: Result<anyhow::Result<serde_json::Value>, _> = tokio::select! {
        res = body => Ok(res),
        _ = stop.cancelled() => Err(anyhow::anyhow!("script stopped")),
    };

    let mut outcome = ScriptOutcome::default();
    match result {
        Err(stopped) => {
            // Distinguish the engine's own deadline from external stop.
            outcome.timed_out = deadline_fired.load(Ordering::Acquire);
            outcome.error = if outcome.timed_out {
                Some(format!("script exceeded wall-clock timeout of {timeout:?}"))
            } else {
                Some(stopped.to_string())
            };
        }
        Ok(Err(err)) => {
            // A deadline-interrupted CPU loop surfaces here as a QuickJS
            // exception from inside the body, not via the stop branch.
            outcome.timed_out = deadline_fired.load(Ordering::Acquire);
            outcome.error = if outcome.timed_out {
                Some(format!("script exceeded wall-clock timeout of {timeout:?}"))
            } else {
                Some(err.to_string())
            };
        }
        Ok(Ok(value)) => {
            let serialized = serde_json::to_string(&value)
                .unwrap_or_else(|_| "\"<unserializable return value>\"".to_string());
            if serialized.len() > limits.max_output_bytes {
                outcome.error = Some(format!(
                    "script return value exceeds output cap ({} > {} bytes)",
                    serialized.len(),
                    limits.max_output_bytes
                ));
            } else {
                outcome.value = Some(value);
            }
        }
    }

    if outcome.error.is_some() || logs.lock().expect("log mutex poisoned").truncated() {
        let mut buffer = logs.lock().expect("log mutex poisoned");
        if buffer.truncated() {
            buffer.push(limits, "[log truncated: cap reached]".to_string());
        }
    }

    Ok(outcome)
}

fn tool_catalog_entry(tool: &Arc<dyn Tool>) -> serde_json::Value {
    json!({
        "name": tool.name(),
        "description": tool.description(),
        "inputSchema": tool.parameters_schema(),
    })
}

fn exception_message(ctx: &rquickjs::Ctx<'_>, err: &rquickjs::Error) -> String {
    let pending = ctx.catch();
    // JS exceptions are usually Error objects with a `message` property.
    let detail = if let Some(text) = pending.as_string().and_then(|s| s.to_string().ok()) {
        Some(text)
    } else if let Some(object) = pending.as_object() {
        object
            .get::<_, String>("message")
            .ok()
            .filter(|message| !message.is_empty())
    } else {
        None
    };
    match detail {
        Some(detail) => format!("{err}: {detail}"),
        None => err.to_string(),
    }
}

/// Install the `ctx` global: `ctx.mcp.*`/`ctx.runtime.*` namespaces,
/// `ctx.harness` metadata, and `ctx.log`.
fn install_context<'js>(
    ctx: &rquickjs::Ctx<'js>,
    by_name: Arc<HashMap<String, Arc<dyn Tool>>>,
    catalog: &str,
    harness: &str,
    logs: &Arc<Mutex<LogBuffer>>,
    limits: &ScriptLimits,
) -> rquickjs::Result<()> {
    ctx.globals().set("__nenjo_tool_catalog", catalog)?;
    ctx.globals().set("__nenjo_harness", harness)?;
    ctx.globals().set(
        "__nenjo_dispatch",
        Function::new(
            ctx.clone(),
            Async(move |name: String, args_json: String| {
                let by_name = by_name.clone();
                async move { dispatch_by_name(&by_name, &name, &args_json).await }
            }),
        )?,
    )?;

    let log_buffer = logs.clone();
    let log_limits = limits.clone();
    ctx.globals().set(
        "__nenjo_log",
        Function::new(ctx.clone(), move |parts: Rest<String>| {
            let line = parts.iter().cloned().collect::<Vec<String>>().join(" ");
            log_buffer
                .lock()
                .expect("log mutex poisoned")
                .push(&log_limits, line);
        })?,
    )?;

    let _: () = ctx.eval(
        r#"
        (() => {
            const catalog = JSON.parse(globalThis.__nenjo_tool_catalog);
            const dispatch = globalThis.__nenjo_dispatch;
            const buildNamespace = (entries) => {
                const ns = {};
                for (const meta of entries) {
                    ns[meta.name] = async (args) =>
                        JSON.parse(await dispatch(meta.name, JSON.stringify(args ?? null)));
                }
                ns.list = () => entries;
                return ns;
            };
            const ctxObj = {
                mcp: buildNamespace(catalog.mcp),
                runtime: buildNamespace(catalog.runtime),
                harness: JSON.parse(globalThis.__nenjo_harness),
                log: (...parts) => globalThis.__nenjo_log(...parts),
            };
            globalThis.ctx = ctxObj;
        })();
    "#,
    )?;
    Ok(())
}

/// Execute one tool call by name through the normal tool pipeline.
///
/// Denials, admission rejections, and rate limits surface as
/// `{ ok: false, error }` — identical to what the model would see directly.
async fn dispatch_by_name(
    tools: &HashMap<String, Arc<dyn Tool>>,
    name: &str,
    args_json: &str,
) -> String {
    let result = match tools.get(name) {
        Some(tool) => {
            let args: serde_json::Value = serde_json::from_str(args_json)
                .unwrap_or(serde_json::Value::String(args_json.to_string()));
            match tool.execute(args).await {
                Ok(result) => result,
                Err(err) => ToolResult::failure(err.to_string()),
            }
        }
        None => ToolResult::failure(format!("unknown tool `{name}` in namespace")),
    };
    let payload = match result.success {
        true => json!({
            "ok": true,
            "content": decode_tool_output(result.output.text_content()),
            "error": serde_json::Value::Null,
        }),
        false => json!({
            "ok": false,
            "tool": name,
            "content": decode_tool_output(result.output.text_content()),
            "error": result
                .error
                .unwrap_or_else(|| "tool call failed".to_string()),
        }),
    };
    payload.to_string()
}

/// Decode tool output for scripts: JSON objects and arrays arrive as
/// structured values the script can use directly; everything else stays
/// text. Bare JSON scalars are deliberately not decoded so numeric or
/// boolean-looking text does not silently change type.
fn decode_tool_output(text: String) -> serde_json::Value {
    match serde_json::from_str::<serde_json::Value>(&text) {
        Ok(value @ (serde_json::Value::Object(_) | serde_json::Value::Array(_))) => value,
        _ => serde_json::Value::String(text),
    }
}

/// Convert a JS value to `serde_json::Value`.
fn js_to_json(value: &Value<'_>) -> serde_json::Value {
    match value.type_of() {
        rquickjs::Type::Undefined | rquickjs::Type::Null | rquickjs::Type::Uninitialized => {
            serde_json::Value::Null
        }
        rquickjs::Type::Bool => serde_json::Value::Bool(
            value
                .as_bool()
                .expect("type_of guarantees bool for Type::Bool"),
        ),
        rquickjs::Type::Int => serde_json::Value::Number(Number::from(
            value
                .as_int()
                .expect("type_of guarantees int for Type::Int"),
        )),
        rquickjs::Type::Float => serde_json::Number::from_f64(
            value
                .as_float()
                .expect("type_of guarantees float for Type::Float"),
        )
        .map_or(serde_json::Value::Null, serde_json::Value::Number),
        rquickjs::Type::String => serde_json::Value::String(
            value
                .as_string()
                .expect("type_of guarantees string for Type::String")
                .to_string()
                .unwrap_or_default(),
        ),
        rquickjs::Type::Array => {
            let array = value
                .as_array()
                .expect("type_of guarantees array for Type::Array");
            let mut items = Vec::with_capacity(array.len());
            for index in 0..array.len() {
                match array.get::<Value>(index) {
                    Ok(item) => items.push(js_to_json(&item)),
                    Err(_) => items.push(serde_json::Value::Null),
                }
            }
            serde_json::Value::Array(items)
        }
        rquickjs::Type::Object => {
            let object = value
                .as_object()
                .expect("type_of guarantees object for Type::Object");
            let mut map = Map::new();
            for entry in object.props::<String, Value>() {
                match entry {
                    Ok((key, item)) => {
                        map.insert(key, js_to_json(&item));
                    }
                    Err(_) => continue,
                }
            }
            serde_json::Value::Object(map)
        }
        // Functions, symbols, and other exotic values have no JSON form.
        _ => serde_json::Value::Null,
    }
}
