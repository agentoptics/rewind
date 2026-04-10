use axum::{
    http::{header, StatusCode, Uri},
    response::{Html, IntoResponse, Response},
};
use rust_embed::Embed;

#[derive(Embed)]
#[folder = "../../web/dist/"]
struct Assets;

pub async fn static_handler(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');

    if let Some(asset) = Assets::get(path) {
        let mime = mime_guess::from_path(path).first_or_octet_stream();
        (
            StatusCode::OK,
            [(header::CONTENT_TYPE, mime.as_ref())],
            asset.data.to_vec(),
        )
            .into_response()
    } else if let Some(index) = Assets::get("index.html") {
        Html(String::from_utf8_lossy(&index.data).to_string()).into_response()
    } else {
        (StatusCode::OK, Html(fallback_html())).into_response()
    }
}

fn fallback_html() -> String {
    r#"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>Rewind — Web UI</title>
    <style>
        body { font-family: system-ui, sans-serif; background: #0a0a0a; color: #e5e5e5; display: flex; align-items: center; justify-content: center; min-height: 100vh; margin: 0; }
        .container { text-align: center; max-width: 480px; }
        h1 { font-size: 2rem; margin-bottom: 0.5rem; }
        .subtitle { color: #a3a3a3; margin-bottom: 2rem; }
        code { background: #1a1a1a; padding: 0.5rem 1rem; border-radius: 0.5rem; display: block; margin: 1rem 0; color: #22d3ee; }
    </style>
</head>
<body>
    <div class="container">
        <h1>⏪ Rewind</h1>
        <p class="subtitle">Web UI not built yet</p>
        <p>Build the frontend first:</p>
        <code>cd web && npm install && npm run build</code>
        <p>Then restart the server.</p>
        <p style="margin-top: 2rem; color: #a3a3a3;">API is live at <a href="/api/health" style="color: #22d3ee;">/api/health</a></p>
    </div>
</body>
</html>"#.to_string()
}
