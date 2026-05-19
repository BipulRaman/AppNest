// API Mock server.
//
// Given the path to a Swagger 2.0 / OpenAPI 3.x JSON file, build an axum
// Router that:
//   * serves Swagger UI at `/`
//   * serves the raw spec at `/swagger.json`
//   * registers one mock route per `paths` x `method` declared in the spec,
//     returning a JSON response synthesised from the operation's example /
//     schema. Path parameters (`{id}`) are converted to axum syntax (`:id`)
//     and ignored at runtime (the same canned body is returned regardless).
//
// The mock is intentionally minimal — no auth, no validation, no stateful
// behaviour — just enough to let a frontend developer point at a contract
// and start coding before the real backend exists.

use axum::{
    body::Body,
    http::{header, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::{get, on, MethodFilter, MethodRouter},
    Router,
};
use serde_json::{json, Map, Value};
use std::sync::Arc;

const SWAGGER_UI_HTML: &str = r#"<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8"/>
  <title>API Mock — Swagger UI</title>
  <link rel="stylesheet" href="/__appnest/swagger-ui.css"/>
  <style>body{margin:0}</style>
</head>
<body>
  <div id="swagger-ui"></div>
  <script src="/__appnest/swagger-ui-bundle.js"></script>
  <script>
    window.ui = SwaggerUIBundle({
      url: '/swagger.json',
      dom_id: '#swagger-ui',
      deepLinking: true,
      presets: [SwaggerUIBundle.presets.apis],
    });
  </script>
</body>
</html>"#;

/// Locally embedded Swagger UI assets. We ship them inside the AppNest
/// binary (via `include_bytes!`) and serve them under `/__appnest/...` so
/// the mock dashboard works with no network access. The `__appnest`
/// prefix is unlikely to collide with any user-defined route in a spec.
const SWAGGER_UI_CSS: &[u8] = include_bytes!("../public/swagger-ui.css");
const SWAGGER_UI_JS: &[u8] = include_bytes!("../public/swagger-ui-bundle.js");

/// Build an axum Router that serves Swagger UI and mock endpoints derived
/// from the spec at `spec_path`.
pub fn build(spec_path: &str) -> Result<Router, String> {
    let raw = std::fs::read_to_string(spec_path)
        .map_err(|e| format!("read swagger spec: {}", e))?;
    let spec: Value = serde_json::from_str(&raw)
        .map_err(|e| format!("parse swagger spec: {}", e))?;

    let spec_arc = Arc::new(raw);
    let spec_for_route = spec_arc.clone();

    let mut router: Router = Router::new()
        .route("/", get(|| async { Html(SWAGGER_UI_HTML) }))
        .route(
            "/__appnest/swagger-ui.css",
            get(|| async {
                ([(header::CONTENT_TYPE, "text/css")], SWAGGER_UI_CSS)
            }),
        )
        .route(
            "/__appnest/swagger-ui-bundle.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "application/javascript")],
                    SWAGGER_UI_JS,
                )
            }),
        )
        .route(
            "/swagger.json",
            get(move || {
                let s = spec_for_route.clone();
                async move {
                    (
                        [(header::CONTENT_TYPE, "application/json")],
                        (*s).clone(),
                    )
                }
            }),
        );

    // basePath (Swagger 2.0) or first server URL path (OpenAPI 3.x).
    let base = extract_base_path(&spec);

    if let Some(paths) = spec.get("paths").and_then(|v| v.as_object()) {
        for (path_str, item) in paths {
            let item_obj = match item.as_object() {
                Some(o) => o,
                None => continue,
            };
            let full = format!("{}{}", base, path_str);
            let axum_path = convert_path(&full);

            let mut mr: Option<MethodRouter> = None;
            for (method, op) in item_obj {
                let m = method.to_ascii_lowercase();
                let filter = match m.as_str() {
                    "get" => MethodFilter::GET,
                    "post" => MethodFilter::POST,
                    "put" => MethodFilter::PUT,
                    "delete" => MethodFilter::DELETE,
                    "patch" => MethodFilter::PATCH,
                    "options" => MethodFilter::OPTIONS,
                    "head" => MethodFilter::HEAD,
                    "trace" => MethodFilter::TRACE,
                    _ => continue,
                };
                let (status, body) = build_mock_response(op, &spec);
                let body_arc = Arc::new(body);
                let handler = move || {
                    let b = body_arc.clone();
                    async move { mock_response(status, &b) }
                };
                let new_mr = on(filter, handler);
                mr = Some(match mr {
                    Some(existing) => existing.merge(new_mr),
                    None => new_mr,
                });
            }
            if let Some(mr) = mr {
                // Defensive: axum panics on duplicate routes. A malformed
                // spec with two identical normalised paths shouldn't crash
                // the whole AppNest process — catch the panic and skip.
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    Router::new().route(&axum_path, mr)
                }));
                if let Ok(sub) = r {
                    router = router.merge(sub);
                }
            }
        }
    }

    Ok(router)
}

// Each axum routing helper (get, post, …) is generic over its handler, so we
// can't store them in a uniform `fn` pointer table. The dispatch above uses
// `on(MethodFilter, handler)` which sidesteps that monomorphisation issue.

fn mock_response(status: StatusCode, body: &Value) -> Response {
    // 204 No Content / 304 Not Modified must not carry a body.
    if status == StatusCode::NO_CONTENT || status == StatusCode::NOT_MODIFIED {
        return Response::builder()
            .status(status)
            .body(Body::empty())
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
    }
    let bytes = serde_json::to_vec(body).unwrap_or_else(|_| b"null".to_vec());
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .header("x-appnest-mock", "1")
        .body(Body::from(bytes))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

/// Convert Swagger/OpenAPI path syntax (`/pets/{id}`) to axum 0.7 syntax
/// (`/pets/:id`).
fn convert_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    let mut i = 0;
    let bytes = path.as_bytes();
    while i < bytes.len() {
        if bytes[i] == b'{' {
            if let Some(end) = path[i + 1..].find('}') {
                let name = &path[i + 1..i + 1 + end];
                out.push(':');
                // Sanitize: axum param names should be `[A-Za-z_][A-Za-z0-9_]*`.
                for ch in name.chars() {
                    if ch.is_ascii_alphanumeric() || ch == '_' {
                        out.push(ch);
                    }
                }
                i = i + 1 + end + 1;
                continue;
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

fn extract_base_path(spec: &Value) -> String {
    // Swagger 2.0
    if let Some(bp) = spec.get("basePath").and_then(|v| v.as_str()) {
        return bp.trim_end_matches('/').to_string();
    }
    // OpenAPI 3.x: first server URL — keep only its path component.
    if let Some(servers) = spec.get("servers").and_then(|v| v.as_array()) {
        if let Some(url) = servers
            .first()
            .and_then(|s| s.get("url"))
            .and_then(|v| v.as_str())
        {
            return path_of_url(url).trim_end_matches('/').to_string();
        }
    }
    String::new()
}

fn path_of_url(url: &str) -> String {
    // If it's already a relative path, use as-is.
    if !url.contains("://") {
        return url.to_string();
    }
    // Strip scheme + authority.
    if let Some(rest) = url.splitn(2, "://").nth(1) {
        if let Some(slash) = rest.find('/') {
            return rest[slash..].to_string();
        }
    }
    String::new()
}

// ─── Mock body synthesis ────────────────────────────────────────────

/// Pick a (status, body) pair from an operation's `responses` map.
fn build_mock_response(op: &Value, root: &Value) -> (StatusCode, Value) {
    let responses = match op.get("responses").and_then(|v| v.as_object()) {
        Some(r) => r,
        None => return (StatusCode::OK, Value::Null),
    };

    // Prefer 200, then 201, then any 2xx, then "default", then anything.
    let chosen_key = ["200", "201", "202", "203", "204"]
        .iter()
        .find(|k| responses.contains_key(**k))
        .map(|s| s.to_string())
        .or_else(|| {
            responses
                .keys()
                .find(|k| k.starts_with('2'))
                .cloned()
        })
        .or_else(|| {
            responses
                .keys()
                .find(|k| *k == "default")
                .cloned()
        })
        .or_else(|| responses.keys().next().cloned())
        .unwrap_or_else(|| "200".to_string());

    let status = chosen_key
        .parse::<u16>()
        .ok()
        .and_then(|n| StatusCode::from_u16(n).ok())
        .unwrap_or(StatusCode::OK);

    let resp_obj = match responses.get(&chosen_key).and_then(|v| resolve_ref(v, root).as_object().cloned()) {
        Some(o) => o,
        None => return (status, Value::Null),
    };

    // OpenAPI 3.x: response.content["application/json"].{example|examples|schema}
    if let Some(content) = resp_obj.get("content").and_then(|v| v.as_object()) {
        let media = content
            .get("application/json")
            .or_else(|| content.values().next())
            .cloned();
        if let Some(media) = media {
            if let Some(ex) = media.get("example") {
                return (status, ex.clone());
            }
            if let Some(examples) = media.get("examples").and_then(|v| v.as_object()) {
                if let Some(first) = examples.values().next() {
                    if let Some(v) = first.get("value") {
                        return (status, v.clone());
                    }
                }
            }
            if let Some(schema) = media.get("schema") {
                return (status, mock_from_schema(schema, root, 0));
            }
        }
    }

    // Swagger 2.0: response.{examples|schema}
    if let Some(examples) = resp_obj.get("examples").and_then(|v| v.as_object()) {
        if let Some(v) = examples
            .get("application/json")
            .or_else(|| examples.values().next())
        {
            return (status, v.clone());
        }
    }
    if let Some(schema) = resp_obj.get("schema") {
        return (status, mock_from_schema(schema, root, 0));
    }

    (status, Value::Null)
}

const MAX_DEPTH: usize = 8;

/// Synthesise a JSON example value from a JSON Schema fragment.
fn mock_from_schema(schema: &Value, root: &Value, depth: usize) -> Value {
    if depth > MAX_DEPTH {
        return Value::Null;
    }
    let schema = resolve_ref(schema, root);

    if let Some(ex) = schema.get("example") {
        return ex.clone();
    }
    if let Some(d) = schema.get("default") {
        return d.clone();
    }
    if let Some(en) = schema.get("enum").and_then(|v| v.as_array()) {
        if let Some(first) = en.first() {
            return first.clone();
        }
    }

    // allOf: merge object properties shallowly.
    if let Some(all) = schema.get("allOf").and_then(|v| v.as_array()) {
        let mut merged = Map::new();
        for s in all {
            let v = mock_from_schema(s, root, depth + 1);
            if let Value::Object(o) = v {
                for (k, val) in o {
                    merged.insert(k, val);
                }
            }
        }
        return Value::Object(merged);
    }
    // oneOf / anyOf: pick the first.
    for key in ["oneOf", "anyOf"] {
        if let Some(arr) = schema.get(key).and_then(|v| v.as_array()) {
            if let Some(first) = arr.first() {
                return mock_from_schema(first, root, depth + 1);
            }
        }
    }

    let ty = schema
        .get("type")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| {
            // OpenAPI 3.1 allows type as array; take the first non-null.
            schema
                .get("type")
                .and_then(|v| v.as_array())
                .and_then(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str())
                        .find(|s| *s != "null")
                        .map(|s| s.to_string())
                })
        })
        .unwrap_or_else(|| {
            if schema.get("properties").is_some() {
                "object".into()
            } else if schema.get("items").is_some() {
                "array".into()
            } else {
                "string".into()
            }
        });

    match ty.as_str() {
        "object" => {
            let mut out = Map::new();
            if let Some(props) = schema.get("properties").and_then(|v| v.as_object()) {
                for (k, v) in props {
                    out.insert(k.clone(), mock_from_schema(v, root, depth + 1));
                }
            }
            Value::Object(out)
        }
        "array" => {
            let item = schema
                .get("items")
                .map(|s| mock_from_schema(s, root, depth + 1))
                .unwrap_or(Value::Null);
            Value::Array(vec![item])
        }
        "string" => {
            let fmt = schema.get("format").and_then(|v| v.as_str()).unwrap_or("");
            let sample = match fmt {
                "date-time" => "2024-01-01T00:00:00Z",
                "date" => "2024-01-01",
                "uuid" => "00000000-0000-0000-0000-000000000000",
                "email" => "user@example.com",
                "uri" | "url" => "https://example.com",
                "byte" => "U3dhZ2dlciByb2Nrcw==",
                "password" => "password",
                _ => "string",
            };
            json!(sample)
        }
        "integer" => json!(0),
        "number" => json!(0.0),
        "boolean" => json!(true),
        "null" => Value::Null,
        _ => Value::Null,
    }
}

/// Follow a `$ref` once (and recursively if the target is itself a ref).
/// Supports `#/components/schemas/X`, `#/definitions/X`, `#/components/responses/X`.
/// Returns the original value unchanged if no ref is present or the ref can't
/// be resolved.
fn resolve_ref(value: &Value, root: &Value) -> Value {
    let mut current = value.clone();
    for _ in 0..16 {
        let ref_path = current
            .as_object()
            .and_then(|o| o.get("$ref"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let Some(ref_path) = ref_path else { return current };
        let Some(stripped) = ref_path.strip_prefix("#/") else { return current };
        let mut node = root;
        let mut ok = true;
        for seg in stripped.split('/') {
            let key = seg.replace("~1", "/").replace("~0", "~");
            match node.get(&key) {
                Some(n) => node = n,
                None => {
                    ok = false;
                    break;
                }
            }
        }
        if !ok {
            return Value::Null;
        }
        current = node.clone();
    }
    current
}
