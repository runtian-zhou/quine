use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PersistedPythonState {
    #[serde(default)]
    pub globals: Map<String, Value>,
    #[serde(default)]
    pub function_definitions: Vec<PersistedPythonFunctionDefinition>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PersistedPythonFunctionDefinition {
    pub name: String,
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PythonExecRequest {
    #[serde(default)]
    pub code: Option<String>,
    #[serde(default)]
    pub function: Option<String>,
    #[serde(default)]
    pub args: Vec<Value>,
    #[serde(default)]
    pub kwargs: Map<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PythonSymbolSummary {
    pub name: String,
    pub kind: String,
    pub type_name: String,
    pub persisted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PythonMethodSummary {
    pub name: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub signature: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PythonInspectResult {
    pub name: String,
    pub kind: String,
    pub type_name: String,
    #[serde(default)]
    pub repr: String,
    #[serde(default)]
    pub value: Option<Value>,
    pub persisted: bool,
    #[serde(default)]
    pub signature: Option<String>,
    #[serde(default)]
    pub docstring: Option<String>,
    #[serde(default)]
    pub owner: Option<String>,
    #[serde(default)]
    pub exposed_methods: Vec<PythonMethodSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PythonListGlobalsResult {
    pub group: String,
    #[serde(default)]
    pub variables: Vec<PythonSymbolSummary>,
    #[serde(default)]
    pub callables: Vec<PythonSymbolSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PythonExecResult {
    pub group: String,
    #[serde(default)]
    pub stdout: String,
    #[serde(default)]
    pub stderr: String,
    #[serde(default)]
    pub result: Option<Value>,
    #[serde(default)]
    pub result_repr: Option<String>,
    #[serde(default)]
    pub non_persisted_globals: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum PythonRuntimeError {
    #[error("python runtime unavailable: {message}")]
    Unavailable { message: String },
    #[error("python execution failed: {message}")]
    ExecutionFailed { message: String },
    #[error("invalid request: {message}")]
    InvalidRequest { message: String },
    #[error("python runtime protocol error: {message}")]
    Protocol { message: String },
}

#[derive(Debug, Default)]
pub struct PythonRuntime {
    groups: Mutex<HashMap<String, Arc<PythonGroup>>>,
}

#[derive(Debug)]
struct PythonGroup {
    state: Mutex<PythonGroupState>,
}

#[derive(Debug, Default)]
struct PythonGroupState {
    process: Option<PythonProcess>,
    persisted: PersistedPythonState,
}

#[derive(Debug)]
struct PythonProcess {
    child: Child,
    stdin: ChildStdin,
    stdout: Lines<BufReader<ChildStdout>>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum WorkerRequest<'a> {
    Restore {
        globals: &'a Map<String, Value>,
        function_definitions: &'a [PersistedPythonFunctionDefinition],
    },
    Exec {
        code: Option<&'a str>,
        function: Option<&'a str>,
        args: &'a [Value],
        kwargs: &'a Map<String, Value>,
    },
    ListGlobals,
    Inspect {
        name: &'a str,
    },
    Snapshot,
}

#[derive(Debug, Deserialize)]
struct WorkerResponse {
    ok: bool,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    stdout: String,
    #[serde(default)]
    stderr: String,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    result_repr: Option<String>,
    #[serde(default)]
    variables: Vec<PythonSymbolSummary>,
    #[serde(default)]
    callables: Vec<PythonSymbolSummary>,
    #[serde(default)]
    inspect: Option<PythonInspectResult>,
    #[serde(default)]
    globals: Map<String, Value>,
    #[serde(default)]
    function_definitions: Vec<PersistedPythonFunctionDefinition>,
    #[serde(default)]
    non_persisted_globals: Vec<String>,
}

impl PythonRuntime {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    async fn group(&self, group: &str) -> Arc<PythonGroup> {
        let mut groups = self.groups.lock().await;
        Arc::clone(
            groups
                .entry(group.to_string())
                .or_insert_with(|| Arc::new(PythonGroup::default())),
        )
    }

    pub async fn restore_group(
        &self,
        group: &str,
        persisted: &PersistedPythonState,
    ) -> Result<(), PythonRuntimeError> {
        let group_handle = self.group(group).await;
        let mut state = group_handle.state.lock().await;
        state.persisted = persisted.clone();
        let globals = state.persisted.globals.clone();
        let function_definitions = state.persisted.function_definitions.clone();
        let response = state
            .request(&WorkerRequest::Restore {
                globals: &globals,
                function_definitions: &function_definitions,
            })
            .await?;
        ensure_ok(&response)?;
        Ok(())
    }

    pub async fn exec(
        &self,
        group: &str,
        request: &PythonExecRequest,
    ) -> Result<PythonExecResult, PythonRuntimeError> {
        if request.code.is_some() == request.function.is_some() {
            return Err(PythonRuntimeError::InvalidRequest {
                message: "provide exactly one of `code` or `function`".into(),
            });
        }
        let group_handle = self.group(group).await;
        let mut state = group_handle.state.lock().await;
        let response = state
            .request(&WorkerRequest::Exec {
                code: request.code.as_deref(),
                function: request.function.as_deref(),
                args: &request.args,
                kwargs: &request.kwargs,
            })
            .await?;
        ensure_ok(&response)?;
        let snapshot = state.request(&WorkerRequest::Snapshot).await?;
        ensure_ok(&snapshot)?;
        state.persisted = PersistedPythonState {
            globals: snapshot.globals.clone(),
            function_definitions: snapshot.function_definitions.clone(),
        };
        Ok(PythonExecResult {
            group: group.to_string(),
            stdout: response.stdout,
            stderr: response.stderr,
            result: response.result,
            result_repr: response.result_repr,
            non_persisted_globals: response.non_persisted_globals,
        })
    }

    pub async fn list_globals(
        &self,
        group: &str,
    ) -> Result<PythonListGlobalsResult, PythonRuntimeError> {
        let group_handle = self.group(group).await;
        let mut state = group_handle.state.lock().await;
        let response = state.request(&WorkerRequest::ListGlobals).await?;
        ensure_ok(&response)?;
        Ok(PythonListGlobalsResult {
            group: group.to_string(),
            variables: response.variables,
            callables: response.callables,
        })
    }

    pub async fn inspect(
        &self,
        group: &str,
        name: &str,
    ) -> Result<PythonInspectResult, PythonRuntimeError> {
        let group_handle = self.group(group).await;
        let mut state = group_handle.state.lock().await;
        let response = state.request(&WorkerRequest::Inspect { name }).await?;
        ensure_ok(&response)?;
        response
            .inspect
            .ok_or_else(|| PythonRuntimeError::Protocol {
                message: "missing inspect payload".into(),
            })
    }

    pub async fn snapshot_group(
        &self,
        group: &str,
    ) -> Result<PersistedPythonState, PythonRuntimeError> {
        let group_handle = self.group(group).await;
        let mut state = group_handle.state.lock().await;
        let response = state.request(&WorkerRequest::Snapshot).await?;
        ensure_ok(&response)?;
        state.persisted = PersistedPythonState {
            globals: response.globals.clone(),
            function_definitions: response.function_definitions.clone(),
        };
        Ok(state.persisted.clone())
    }
}

impl Default for PythonGroup {
    fn default() -> Self {
        Self {
            state: Mutex::new(PythonGroupState::default()),
        }
    }
}

impl PythonGroupState {
    async fn request<'a>(
        &'a mut self,
        request: &WorkerRequest<'a>,
    ) -> Result<WorkerResponse, PythonRuntimeError> {
        self.ensure_process().await?;
        let response = match self.send_request(request).await {
            Ok(response) => response,
            Err(_) => {
                self.process = None;
                self.ensure_process().await?;
                self.send_request(request).await?
            }
        };
        Ok(response)
    }

    async fn ensure_process(&mut self) -> Result<(), PythonRuntimeError> {
        let needs_spawn = match self.process.as_mut() {
            Some(process) => process
                .child
                .try_wait()
                .map_err(|error| PythonRuntimeError::Unavailable {
                    message: error.to_string(),
                })?
                .is_some(),
            None => true,
        };

        if !needs_spawn {
            return Ok(());
        }

        self.process = Some(spawn_worker().await?);
        if !self.persisted.globals.is_empty() || !self.persisted.function_definitions.is_empty() {
            let globals = self.persisted.globals.clone();
            let function_definitions = self.persisted.function_definitions.clone();
            let restore = WorkerRequest::Restore {
                globals: &globals,
                function_definitions: &function_definitions,
            };
            let response = self.send_request(&restore).await?;
            ensure_ok(&response)?;
        }
        Ok(())
    }

    async fn send_request<'a>(
        &'a mut self,
        request: &WorkerRequest<'a>,
    ) -> Result<WorkerResponse, PythonRuntimeError> {
        let process = self
            .process
            .as_mut()
            .ok_or_else(|| PythonRuntimeError::Unavailable {
                message: "python worker is not running".into(),
            })?;
        let mut line =
            serde_json::to_string(request).map_err(|error| PythonRuntimeError::Protocol {
                message: error.to_string(),
            })?;
        line.push('\n');
        process
            .stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|error| PythonRuntimeError::Unavailable {
                message: error.to_string(),
            })?;
        process
            .stdin
            .flush()
            .await
            .map_err(|error| PythonRuntimeError::Unavailable {
                message: error.to_string(),
            })?;
        let line = process
            .stdout
            .next_line()
            .await
            .map_err(|error| PythonRuntimeError::Protocol {
                message: error.to_string(),
            })?
            .ok_or_else(|| PythonRuntimeError::Unavailable {
                message: "python worker exited".into(),
            })?;
        serde_json::from_str(&line).map_err(|error| PythonRuntimeError::Protocol {
            message: error.to_string(),
        })
    }
}

async fn spawn_worker() -> Result<PythonProcess, PythonRuntimeError> {
    let commands = ["python3", "python"];
    let mut last_error = None;
    for program in commands {
        let mut child = match Command::new(program)
            .arg("-u")
            .arg("-c")
            .arg(PYTHON_WORKER)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(child) => child,
            Err(error) => {
                last_error = Some(error.to_string());
                continue;
            }
        };
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| PythonRuntimeError::Unavailable {
                message: "failed to open stdin for python worker".into(),
            })?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| PythonRuntimeError::Unavailable {
                message: "failed to open stdout for python worker".into(),
            })?;
        return Ok(PythonProcess {
            child,
            stdin,
            stdout: BufReader::new(stdout).lines(),
        });
    }

    Err(PythonRuntimeError::Unavailable {
        message: last_error.unwrap_or_else(|| "python interpreter not found".into()),
    })
}

fn ensure_ok(response: &WorkerResponse) -> Result<(), PythonRuntimeError> {
    if response.ok {
        Ok(())
    } else {
        Err(PythonRuntimeError::ExecutionFailed {
            message: response
                .error
                .clone()
                .unwrap_or_else(|| "unknown python execution failure".into()),
        })
    }
}

const PYTHON_WORKER: &str = r#"
import ast
import contextlib
import inspect
import io
import json
import linecache
import sys
import traceback
import types

GLOBAL_STATE = {"__builtins__": __builtins__}
FUNCTION_SOURCES = {}
CODE_COUNTER = 0

INTERNAL_NAMES = {"__builtins__"}

def json_value(obj):
    try:
        json.dumps(obj)
        return obj, True
    except Exception:
        return None, False

def public_globals():
    return {
        name: value
        for name, value in GLOBAL_STATE.items()
        if name not in INTERNAL_NAMES and not name.startswith("__")
    }

def kind_of(obj):
    if inspect.ismethod(obj):
        return "bound_method"
    if inspect.isfunction(obj) or inspect.isbuiltin(obj):
        return "function"
    if inspect.isclass(obj):
        return "class"
    if inspect.ismodule(obj):
        return "module"
    return "variable"

def can_persist(name, value):
    _, persisted = json_value(value)
    if persisted:
        return True
    return inspect.isfunction(value) and name in FUNCTION_SOURCES

def symbol_summary(name, value):
    return {
        "name": name,
        "kind": kind_of(value),
        "type_name": type(value).__name__,
        "persisted": can_persist(name, value),
    }

def trim(text, limit=400):
    if text is None:
        return None
    text = str(text)
    if len(text) <= limit:
        return text
    return text[: limit - 3] + "..."

def inspect_callable(obj):
    try:
        return trim(str(inspect.signature(obj)), 200)
    except Exception:
        return None

def inspect_methods(obj):
    methods = []
    for name in dir(obj):
        if name.startswith("_"):
            continue
        try:
            value = getattr(obj, name)
        except Exception:
            continue
        if callable(value):
            methods.append({
                "name": name,
                "kind": kind_of(value),
                "signature": inspect_callable(value),
            })
    methods.sort(key=lambda item: item["name"])
    return methods

def inspect_symbol(path):
    target = resolve_name(path)
    value_json, persisted = json_value(target)
    persisted = persisted or (inspect.isfunction(target) and path in FUNCTION_SOURCES)
    owner = None
    if inspect.ismethod(target):
        owner = type(target.__self__).__name__
    elif hasattr(target, "__self__") and target.__self__ is not None:
        owner = type(target.__self__).__name__
    return {
        "name": path,
        "kind": kind_of(target),
        "type_name": type(target).__name__,
        "repr": trim(repr(target), 400),
        "value": value_json,
        "persisted": persisted,
        "signature": inspect_callable(target) if callable(target) else None,
        "docstring": trim(inspect.getdoc(target), 400),
        "owner": owner,
        "exposed_methods": inspect_methods(target) if inspect.isclass(target) or (not callable(target) and hasattr(target, "__dict__")) else [],
    }

def resolve_name(path):
    parts = path.split(".")
    if not parts or not parts[0]:
        raise KeyError("empty name")
    if parts[0] not in GLOBAL_STATE:
        raise KeyError(parts[0])
    current = GLOBAL_STATE[parts[0]]
    for part in parts[1:]:
        current = getattr(current, part)
    return current

def snapshot_globals():
    persisted = {}
    for name, value in public_globals().items():
        encoded, ok = json_value(value)
        if ok:
            persisted[name] = encoded
    return persisted

def snapshot_function_definitions():
    persisted = []
    for name, value in public_globals().items():
        if inspect.isfunction(value) and value.__name__ == name and name in FUNCTION_SOURCES:
            persisted.append({
                "name": name,
                "source": FUNCTION_SOURCES[name],
            })
    persisted.sort(key=lambda item: item["name"])
    return persisted

def non_persisted_globals():
    names = []
    for name, value in public_globals().items():
        if not can_persist(name, value):
            names.append(name)
    names.sort()
    return names

def record_function_sources(code, filename):
    global CODE_COUNTER
    try:
        module = ast.parse(code, filename=filename, mode="exec")
    except SyntaxError:
        return
    for node in module.body:
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
            segment = ast.get_source_segment(code, node)
            if segment:
                FUNCTION_SOURCES[node.name] = segment
        elif isinstance(node, (ast.Assign, ast.AnnAssign)):
            targets = []
            if isinstance(node, ast.Assign):
                targets = [target.id for target in node.targets if isinstance(target, ast.Name)]
            elif isinstance(node, ast.AnnAssign) and isinstance(node.target, ast.Name):
                targets = [node.target.id]
            for target in targets:
                FUNCTION_SOURCES.pop(target, None)

def handle_exec(request):
    global CODE_COUNTER
    stdout = io.StringIO()
    stderr = io.StringIO()
    result = None
    result_repr = None
    with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
        if request.get("code") is not None:
            code = request["code"]
            try:
                compiled = compile(code, "<quine-python>", "eval")
            except SyntaxError:
                compiled = None
            if compiled is not None:
                value = eval(compiled, GLOBAL_STATE)
                result, ok = json_value(value)
                if not ok:
                    result = None
                    result_repr = trim(repr(value), 400)
            else:
                filename = f"<quine-python-{CODE_COUNTER}>"
                CODE_COUNTER += 1
                linecache.cache[filename] = (len(code), None, [line + "\n" for line in code.splitlines()], filename)
                exec(compile(code, filename, "exec"), GLOBAL_STATE)
                record_function_sources(code, filename)
        else:
            function = resolve_name(request["function"])
            if not callable(function):
                raise TypeError(f"{request['function']} is not callable")
            value = function(*request.get("args", []), **request.get("kwargs", {}))
            result, ok = json_value(value)
            if not ok:
                result = None
                result_repr = trim(repr(value), 400)
    return {
        "ok": True,
        "stdout": stdout.getvalue(),
        "stderr": stderr.getvalue(),
        "result": result,
        "result_repr": result_repr,
        "non_persisted_globals": non_persisted_globals(),
    }

def handle_request(request):
    kind = request["kind"]
    if kind == "restore":
        GLOBAL_STATE.clear()
        GLOBAL_STATE["__builtins__"] = __builtins__
        FUNCTION_SOURCES.clear()
        GLOBAL_STATE.update(request.get("globals", {}))
        for item in request.get("function_definitions", []):
            exec(compile(item["source"], f"<quine-restore-{item['name']}>", "exec"), GLOBAL_STATE)
            FUNCTION_SOURCES[item["name"]] = item["source"]
        return {"ok": True}
    if kind == "exec":
        return handle_exec(request)
    if kind == "list_globals":
        variables = []
        callables = []
        for name, value in sorted(public_globals().items()):
            summary = symbol_summary(name, value)
            if callable(value):
                callables.append(summary)
            else:
                variables.append(summary)
        return {"ok": True, "variables": variables, "callables": callables}
    if kind == "inspect":
        return {"ok": True, "inspect": inspect_symbol(request["name"])}
    if kind == "snapshot":
        return {
            "ok": True,
            "globals": snapshot_globals(),
            "function_definitions": snapshot_function_definitions(),
        }
    raise ValueError(f"unknown request kind: {kind}")

for raw in sys.stdin:
    raw = raw.strip()
    if not raw:
        continue
    try:
        request = json.loads(raw)
        response = handle_request(request)
    except Exception as exc:
        response = {
            "ok": False,
            "error": trim("".join(traceback.format_exception_only(type(exc), exc)).strip(), 400),
        }
    sys.stdout.write(json.dumps(response) + "\n")
    sys.stdout.flush()
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn runtime_exec_and_inspect() {
        let runtime = PythonRuntime::new();
        let exec = runtime
            .exec(
                "g1",
                &PythonExecRequest {
                    code: Some("x = 2\n\ndef add(y):\n    return x + y\n".into()),
                    function: None,
                    args: Vec::new(),
                    kwargs: Map::new(),
                },
            )
            .await
            .unwrap();
        assert!(!exec.non_persisted_globals.contains(&"add".to_string()));

        let globals = runtime.list_globals("g1").await.unwrap();
        assert!(globals.variables.iter().any(|item| item.name == "x"));
        assert!(globals.callables.iter().any(|item| item.name == "add"));

        let inspected = runtime.inspect("g1", "add").await.unwrap();
        assert_eq!(inspected.kind, "function");

        let snapshot = runtime.snapshot_group("g1").await.unwrap();
        assert_eq!(snapshot.globals.get("x"), Some(&Value::from(2)));
        assert_eq!(snapshot.function_definitions.len(), 1);
        assert_eq!(snapshot.function_definitions[0].name, "add");
    }

    #[tokio::test]
    async fn runtime_restore_roundtrip() {
        let runtime = PythonRuntime::new();
        runtime
            .restore_group(
                "g2",
                &PersistedPythonState {
                    globals: Map::from_iter([(String::from("x"), Value::from(5))]),
                    function_definitions: Vec::new(),
                },
            )
            .await
            .unwrap();
        let inspected = runtime.inspect("g2", "x").await.unwrap();
        assert_eq!(inspected.value, Some(Value::from(5)));
    }
}
