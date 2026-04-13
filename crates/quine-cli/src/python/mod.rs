use std::path::Path;

use crate::client::IpcClient;
use quine_harness::protocol::methods;
use serde::Serialize;
use serde_json::{Map, Value};

#[derive(Serialize)]
struct PythonExecRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    function: Option<String>,
    #[serde(default)]
    args: Vec<Value>,
    #[serde(default)]
    kwargs: Map<String, Value>,
}

pub(crate) struct PythonCliOptions<'a> {
    pub session_id: Option<&'a str>,
    pub session_group: Option<&'a str>,
    pub code: Option<&'a str>,
    pub file: Option<&'a str>,
    pub function: Option<&'a str>,
    pub args: &'a [String],
    pub kwargs: &'a [String],
    pub list_globals: bool,
    pub inspect: Option<&'a str>,
    pub json_output: bool,
}

pub(crate) async fn handle_py(
    socket_path: &Path,
    options: PythonCliOptions<'_>,
) -> anyhow::Result<()> {
    let mut client = IpcClient::connect_or_launch(socket_path).await?.0;
    let target = build_target(options.session_id, options.session_group)?;

    let mode_count = usize::from(options.code.is_some())
        + usize::from(options.file.is_some())
        + usize::from(options.function.is_some())
        + usize::from(options.list_globals)
        + usize::from(options.inspect.is_some());
    if mode_count != 1 {
        anyhow::bail!(
            "choose exactly one of inline code, --file, --call, --list-globals, or --inspect"
        );
    }

    if options.list_globals {
        let result = client
            .call(methods::PYTHON_LIST_GLOBALS, Some(target))
            .await?
            .map_err(|error| anyhow::anyhow!(error))?;
        if options.json_output {
            println!("{}", serde_json::to_string_pretty(&result)?);
        } else {
            print_list_globals(&result);
        }
        return Ok(());
    }

    if let Some(name) = options.inspect {
        let mut params = target;
        params["name"] = Value::String(name.to_string());
        let result = client
            .call(methods::PYTHON_INSPECT_GLOBAL, Some(params))
            .await?
            .map_err(|error| anyhow::anyhow!(error))?;
        if options.json_output {
            println!("{}", serde_json::to_string_pretty(&result)?);
        } else {
            print_inspect(&result);
        }
        return Ok(());
    }

    let code = match (options.code, options.file) {
        (Some(code), None) => Some(code.to_string()),
        (None, Some(path)) => Some(tokio::fs::read_to_string(path).await?),
        (None, None) => None,
        (Some(_), Some(_)) => anyhow::bail!("choose either inline code or --file, not both"),
    };

    let request = PythonExecRequest {
        code,
        function: options.function.map(str::to_string),
        args: parse_args(options.args)?,
        kwargs: parse_kwargs(options.kwargs)?,
    };
    let mut params = target;
    let mut request_value = serde_json::to_value(request)?;
    if let Some(request_object) = request_value.as_object_mut() {
        for (key, value) in request_object.iter() {
            params[key] = value.clone();
        }
    }

    let result = client
        .call(methods::PYTHON_EXEC, Some(params))
        .await?
        .map_err(|error| anyhow::anyhow!(error))?;
    if options.json_output {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        print_exec(&result);
    }
    Ok(())
}

fn build_target(session_id: Option<&str>, session_group: Option<&str>) -> anyhow::Result<Value> {
    match (session_id, session_group) {
        (Some(_), Some(_)) => anyhow::bail!("choose either --session or --group, not both"),
        (None, None) => anyhow::bail!("missing --session or --group"),
        (Some(session_id), None) => Ok(serde_json::json!({ "session_id": session_id })),
        (None, Some(session_group)) => Ok(serde_json::json!({ "session_group": session_group })),
    }
}

fn parse_args(values: &[String]) -> anyhow::Result<Vec<Value>> {
    values
        .iter()
        .map(|value| serde_json::from_str(value).map_err(anyhow::Error::from))
        .collect()
}

fn parse_kwargs(values: &[String]) -> anyhow::Result<Map<String, Value>> {
    let mut kwargs = Map::new();
    for item in values {
        let Some((key, value)) = item.split_once('=') else {
            anyhow::bail!("invalid --kw value `{item}`; expected key=<json>");
        };
        kwargs.insert(key.to_string(), serde_json::from_str(value)?);
    }
    Ok(kwargs)
}

fn print_exec(result: &Value) {
    if let Some(stdout) = result
        .get("stdout")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    {
        println!("{stdout}");
    }
    if let Some(stderr) = result
        .get("stderr")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    {
        eprintln!("{stderr}");
    }
    if let Some(value) = result.get("result") {
        if !value.is_null() {
            println!(
                "{}",
                serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
            );
        }
    } else if let Some(repr) = result.get("result_repr").and_then(Value::as_str) {
        println!("{repr}");
    }
    if let Some(non_persisted) = result
        .get("non_persisted_globals")
        .and_then(Value::as_array)
        .filter(|items| !items.is_empty())
    {
        eprintln!(
            "non-persisted globals: {}",
            non_persisted
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
}

fn print_list_globals(result: &Value) {
    if let Some(group) = result.get("group").and_then(Value::as_str) {
        println!("group: {group}");
    }
    println!("variables:");
    for item in result
        .get("variables")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let name = item
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("<unknown>");
        let type_name = item
            .get("type_name")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        println!("- {name} ({type_name})");
    }
    println!("callables:");
    for item in result
        .get("callables")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let name = item
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("<unknown>");
        let kind = item
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or("callable");
        println!("- {name} [{kind}]");
    }
}

fn print_inspect(result: &Value) {
    let name = result
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("<unknown>");
    let kind = result
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let type_name = result
        .get("type_name")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    println!("{name}: {kind} ({type_name})");
    if let Some(signature) = result.get("signature").and_then(Value::as_str) {
        println!("signature: {signature}");
    }
    if let Some(repr) = result.get("repr").and_then(Value::as_str) {
        println!("repr: {repr}");
    }
    if let Some(value) = result.get("value") {
        if !value.is_null() {
            println!(
                "value: {}",
                serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
            );
        }
    }
    if let Some(docstring) = result.get("docstring").and_then(Value::as_str) {
        println!("doc: {docstring}");
    }
    if let Some(methods) = result
        .get("exposed_methods")
        .and_then(Value::as_array)
        .filter(|items| !items.is_empty())
    {
        println!("methods:");
        for method in methods {
            let name = method
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("<unknown>");
            let signature = method
                .get("signature")
                .and_then(Value::as_str)
                .unwrap_or("");
            if signature.is_empty() {
                println!("- {name}");
            } else {
                println!("- {name}{signature}");
            }
        }
    }
}
