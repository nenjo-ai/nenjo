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
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};

use anyhow::Result;
use nenjo_tool_api::{Tool, ToolResult};
use rquickjs::{
    function::Async, function::Rest, AsyncContext, AsyncRuntime, Function, Promise, Value,
};
use serde_json::{json, Map, Number};
use tokio::time::{Duration, Instant};

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

/// Tool namespaces exposed to a script. Phase 1 populates only `mcp`.
#[derive(Clone, Default)]
pub struct ScriptNamespaces {
    pub mcp: Vec<Arc<dyn Tool>>,
}

/// Result of a script run.
#[derive(Debug, Default)]
pub struct ScriptOutcome {
    /// JSON-serializable return value of the script.
    pub value: Option<serde_json::Value>,
    /// Error message when the script threw, aborted, or timed out.
    pub error: Option<String>,
    /// True when the wall-clock budget was exhausted.
    pub timed_out: bool,
    /// Lines emitted through `ctx.log`.
    pub logs: Vec<String>,
}

#[derive(Default)]
struct LogBuffer {
    lines: Vec<String>,
    total_bytes: usize,
    truncated: bool,
}

impl LogBuffer {
    fn push(&mut self, limits: &ScriptLimits, line: String) {
        if self.lines.len() >= limits.max_log_lines
            || self.total_bytes + line.len() > limits.max_log_bytes
        {
            self.truncated = true;
            return;
        }
        self.total_bytes += line.len();
        self.lines.push(line);
    }
}

/// Run a script with the given namespaces and limits.
pub async fn run(
    script: &str,
    namespaces: ScriptNamespaces,
    limits: &ScriptLimits,
    timeout: Duration,
) -> Result<ScriptOutcome> {
    let timeout = timeout.min(limits.max_timeout);
    let logs = Arc::new(Mutex::new(LogBuffer::default()));

    let runtime = AsyncRuntime::new()?;
    runtime.set_memory_limit(limits.max_memory_bytes).await;
    runtime.set_max_stack_size(limits.max_stack_bytes).await;

    // Interrupt handler aborts CPU-bound loops (a blocked interpreter never
    // yields to the scheduler, so a timer on the local executor would not fire
    // — the deadline must live on a dedicated OS thread).
    let deadline = Arc::new(AtomicBool::new(false));
    let expired = deadline.clone();
    runtime
        .set_interrupt_handler(Some(Box::new(move || expired.load(Ordering::Relaxed))))
        .await;
    std::thread::spawn({
        let expired = deadline.clone();
        move || {
            std::thread::sleep(timeout);
            expired.store(true, Ordering::Relaxed);
        }
    });

    let ctx = AsyncContext::full(&runtime).await?;
    let started = Instant::now();

    // Dispatch registry: JS refers to tools by name; native side resolves.
    let mcp_by_name: HashMap<String, Arc<dyn Tool>> = namespaces
        .mcp
        .iter()
        .map(|tool| (tool.name().to_string(), tool.clone()))
        .collect();
    let catalog = json!(namespaces
        .mcp
        .iter()
        .map(|tool| {
            json!({
                "name": tool.name(),
                "description": tool.description(),
                "inputSchema": tool.parameters_schema(),
            })
        })
        .collect::<Vec<_>>())
    .to_string();

    let ctx_logs = logs.clone();
    let result: Result<anyhow::Result<serde_json::Value>, _> = tokio::time::timeout(
        timeout + Duration::from_secs(1),
        rquickjs::async_with!(ctx => |ctx| {
            install_context(&ctx, mcp_by_name, &catalog, &ctx_logs, limits)?;
            let wrapper =
                format!("globalThis.__nenjo_script_result = (async () => {{\n{script}\n}})();");
            let returned: Value = ctx.eval(wrapper.as_str())?;
            let promise = Promise::from_value(returned)?;
            match promise.into_future::<Value>().await {
                Ok(value) => Ok(js_to_json(&value)),
                Err(err) => {
                    let message = exception_message(&ctx, &err);
                    Err(anyhow::anyhow!(message))
                }
            }
        }),
    )
    .await;

    let mut outcome = ScriptOutcome {
        logs: {
            let mut buffer = logs.lock().expect("log mutex poisoned");
            if buffer.truncated {
                buffer.lines.push("[log truncated: cap reached]".to_string());
            }
            std::mem::take(&mut buffer.lines)
        },
        ..ScriptOutcome::default()
    };

    match result {
        Err(_elapsed) => {
            outcome.timed_out = true;
            outcome.error = Some(format!(
                "script exceeded wall-clock timeout of {:?}",
                timeout
            ));
        }
        Ok(Err(err)) => {
            // Deadline-interrupted runs surface as Error::Interrupted.
            outcome.timed_out = started.elapsed() >= timeout || deadline.load(Ordering::Relaxed);
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

    Ok(outcome)
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

/// Install the `ctx` global: `ctx.mcp.<tool>()` methods, `ctx.mcp.list()`,
/// and `ctx.log`, backed by one native async dispatch function and the JS shim.
fn install_context<'js>(
    ctx: &rquickjs::Ctx<'js>,
    mcp_by_name: HashMap<String, Arc<dyn Tool>>,
    catalog: &str,
    logs: &Arc<Mutex<LogBuffer>>,
    limits: &ScriptLimits,
) -> rquickjs::Result<()> {
    let mcp_by_name = Arc::new(mcp_by_name);
    ctx.globals().set("__nenjo_tool_catalog", catalog)?;
    ctx.globals().set(
        "__nenjo_dispatch",
        Function::new(
            ctx.clone(),
            Async(move |name: String, args_json: String| {
                let mcp_by_name = mcp_by_name.clone();
                async move { dispatch_by_name(&mcp_by_name, &name, &args_json).await }
            }),
        )?,
    )?;

    let log_buffer = logs.clone();
    let log_limits = limits.clone();
    ctx.globals().set(
        "__nenjo_log",
        Function::new(
            ctx.clone(),
            move |parts: Rest<String>| {
                let line = parts.iter().cloned().collect::<Vec<String>>().join(" ");
                log_buffer
                    .lock()
                    .expect("log mutex poisoned")
                    .push(&log_limits, line);
            },
        )?,
    )?;

    let _: () = ctx
        .eval(r#"
        (() => {
            const catalog = JSON.parse(globalThis.__nenjo_tool_catalog);
            const dispatch = globalThis.__nenjo_dispatch;
            const ns = {};
            for (const meta of catalog) {
                ns[meta.name] = async (args) =>
                    JSON.parse(await dispatch(meta.name, JSON.stringify(args ?? null)));
            }
            ns.list = () => catalog;
            globalThis.ctx = {
                mcp: ns,
                log: (...parts) => globalThis.__nenjo_log(...parts),
            };
        })();
    "#)?;
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
            let args: serde_json::Value = serde_json::from_str(args_json).unwrap_or(
                serde_json::Value::String(args_json.to_string()),
            );
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
            "content": result.output.text_content(),
            "error": serde_json::Value::Null,
        }),
        false => json!({
            "ok": false,
            "content": serde_json::Value::Null,
            "error": result
                .error
                .unwrap_or_else(|| "tool call failed".to_string()),
        }),
    };
    payload.to_string()
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
            value.as_int().expect("type_of guarantees int for Type::Int"),
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
