use anyhow::Result;
use rusqlite::Connection;
use serde::Serialize;
use std::path::PathBuf;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

// ── Data types ──────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct GraphData {
    nodes: Vec<GraphNode>,
    edges: Vec<GraphEdge>,
}

#[derive(Serialize)]
struct GraphNode {
    id: String,
    name: String,
    kind: String,
    path: String,
    line: i64,
}

#[derive(Serialize)]
struct GraphEdge {
    source: String,
    target: String,
    kind: String,
}

// ── Public API ──────────────────────────────────────────────────────────────

/// Start the 3D graph visualization HTTP server on the given port.
#[allow(dead_code)]
pub async fn start_viz_server(repo_root: PathBuf, port: u16) -> Result<()> {
    let listener = TcpListener::bind(format!("0.0.0.0:{port}")).await?;
    print_viz_banner(port);

    loop {
        let (mut stream, _addr) = listener.accept().await?;
        let repo = repo_root.clone();

        tokio::spawn(async move {
            let mut buf = vec![0u8; 8192];
            let n = match stream.read(&mut buf).await {
                Ok(n) if n > 0 => n,
                _ => return,
            };

            let request = String::from_utf8_lossy(&buf[..n]);
            let request_line = request.lines().next().unwrap_or("");
            let path = request_line
                .split_whitespace()
                .nth(1)
                .unwrap_or("/");

            let response = match path {
                "/" => build_html_response(),
                "/api/graph" => build_graph_response(&repo),
                _ => build_404_response(),
            };

            let _ = stream.write_all(response.as_bytes()).await;
            let _ = stream.flush().await;
        });
    }
}

/// Print the server banner to stderr.
#[allow(dead_code)]
pub fn print_viz_banner(port: u16) {
    eprintln!("Visualization server running at http://localhost:{port}");
}

// ── HTTP response builders ──────────────────────────────────────────────────

fn build_html_response() -> String {
    format!(
        "HTTP/1.1 200 OK\r\n\
         Content-Type: text/html; charset=utf-8\r\n\
         Access-Control-Allow-Origin: *\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\r\n\
         {}",
        VIZ_HTML.len(),
        VIZ_HTML
    )
}

fn build_graph_response(repo_root: &PathBuf) -> String {
    let body = match load_graph_data(repo_root) {
        Ok(data) => serde_json::to_string(&data).unwrap_or_else(|_| r#"{"nodes":[],"edges":[]}"#.to_string()),
        Err(e) => {
            tracing::warn!("Failed to load graph data: {e}");
            r#"{"nodes":[],"edges":[]}"#.to_string()
        }
    };
    format!(
        "HTTP/1.1 200 OK\r\n\
         Content-Type: application/json\r\n\
         Access-Control-Allow-Origin: *\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\r\n\
         {}",
        body.len(),
        body
    )
}

fn build_404_response() -> String {
    let body = "Not Found";
    format!(
        "HTTP/1.1 404 Not Found\r\n\
         Content-Type: text/plain\r\n\
         Access-Control-Allow-Origin: *\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\r\n\
         {}",
        body.len(),
        body
    )
}

// ── Database queries ────────────────────────────────────────────────────────

fn load_graph_data(repo_root: &PathBuf) -> Result<GraphData> {
    let db_path = repo_root.join(".tokenuinely").join("index.db");
    let conn = Connection::open(&db_path)?;

    let nodes = load_nodes(&conn).unwrap_or_default();
    let edges = load_edges(&conn).unwrap_or_default();

    Ok(GraphData { nodes, edges })
}

fn load_nodes(conn: &Connection) -> Result<Vec<GraphNode>> {
    let mut stmt = conn.prepare("SELECT name, kind, path, line_start FROM symbols")?;
    let rows = stmt.query_map([], |row| {
        let name: String = row.get(0)?;
        let kind: String = row.get(1)?;
        let path: String = row.get(2)?;
        let line: i64 = row.get(3)?;
        let id = format!("{path}::{name}");
        Ok(GraphNode {
            id,
            name,
            kind,
            path,
            line,
        })
    })?;

    let mut nodes = Vec::new();
    for row in rows {
        if let Ok(node) = row {
            nodes.push(node);
        }
    }
    Ok(nodes)
}

fn load_edges(conn: &Connection) -> Result<Vec<GraphEdge>> {
    let mut stmt =
        conn.prepare("SELECT source_symbol, target_symbol, kind FROM deps WHERE source_symbol IS NOT NULL")?;
    let rows = stmt.query_map([], |row| {
        Ok(GraphEdge {
            source: row.get(0)?,
            target: row.get(1)?,
            kind: row.get(2)?,
        })
    })?;

    let mut edges = Vec::new();
    for row in rows {
        if let Ok(edge) = row {
            edges.push(edge);
        }
    }
    Ok(edges)
}

// ── Embedded HTML ───────────────────────────────────────────────────────────

const VIZ_HTML: &str = r##"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>tokenuinely — Code Graph</title>
<style>
  * { margin: 0; padding: 0; box-sizing: border-box; }
  body { background: #1a1a2e; overflow: hidden; font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif; }
  #graph { width: 100vw; height: 100vh; }
  #tooltip {
    position: fixed; display: none; padding: 10px 14px; border-radius: 8px;
    background: rgba(20, 20, 40, 0.95); color: #e0e0e0; font-size: 13px;
    pointer-events: none; max-width: 400px; z-index: 100;
    border: 1px solid rgba(255,255,255,0.1); backdrop-filter: blur(8px);
    box-shadow: 0 4px 20px rgba(0,0,0,0.4);
  }
  #tooltip .tt-name { font-weight: 600; font-size: 15px; color: #fff; margin-bottom: 4px; }
  #tooltip .tt-kind { font-size: 11px; text-transform: uppercase; letter-spacing: 0.5px; opacity: 0.7; margin-bottom: 6px; }
  #tooltip .tt-path { font-size: 12px; color: #90caf9; word-break: break-all; }
  #info-panel {
    position: fixed; bottom: 0; left: 0; right: 0; padding: 10px 20px;
    background: rgba(20, 20, 40, 0.85); color: #aaa; font-size: 12px;
    display: flex; justify-content: space-between; align-items: center;
    border-top: 1px solid rgba(255,255,255,0.05);
  }
  #info-panel .title { color: #fff; font-weight: 600; font-size: 14px; }
  #info-panel .stats { opacity: 0.7; }
  #loading {
    position: fixed; top: 50%; left: 50%; transform: translate(-50%, -50%);
    color: #fff; font-size: 18px; z-index: 200;
  }
</style>
</head>
<body>
<div id="graph"></div>
<div id="tooltip">
  <div class="tt-name"></div>
  <div class="tt-kind"></div>
  <div class="tt-path"></div>
</div>
<div id="info-panel">
  <span class="title">tokenuinely — Code Graph</span>
  <span class="stats" id="stats"></span>
</div>
<div id="loading">Loading graph…</div>

<script src="https://unpkg.com/three@0.160.0/build/three.min.js"></script>
<script src="https://unpkg.com/3d-force-graph@1.73.3/dist/3d-force-graph.min.js"></script>
<script>
const KIND_COLORS = {
  'function': '#4CAF50',
  'struct':   '#2196F3',
  'class':    '#2196F3',
  'trait':    '#FF9800',
  'interface':'#FF9800',
  'enum':     '#9C27B0',
  'method':   '#00BCD4',
};
const DEFAULT_COLOR = '#757575';

function colorForKind(kind) {
  const k = (kind || '').toLowerCase();
  return KIND_COLORS[k] || DEFAULT_COLOR;
}

const tooltip = document.getElementById('tooltip');
const loading = document.getElementById('loading');
const statsEl = document.getElementById('stats');

fetch('/api/graph')
  .then(r => r.json())
  .then(data => {
    loading.style.display = 'none';
    statsEl.textContent = data.nodes.length + ' nodes · ' + data.edges.length + ' edges';

    const nodeIds = new Set(data.nodes.map(n => n.id));
    const edges = data.edges.filter(e => nodeIds.has(e.source) && nodeIds.has(e.target));

    const graph = ForceGraph3D()(document.getElementById('graph'))
      .graphData({ nodes: data.nodes, links: edges })
      .nodeId('id')
      .linkSource('source')
      .linkTarget('target')
      .backgroundColor('#1a1a2e')
      .nodeVal(3)
      .nodeColor(n => colorForKind(n.kind))
      .nodeOpacity(0.9)
      .linkWidth(0.4)
      .linkOpacity(0.3)
      .linkColor(e => {
        const k = (e.kind || '').toUpperCase();
        return k === 'IMPORTS' ? '#42a5f5' : '#555';
      })
      .linkDirectionalParticles(1)
      .linkDirectionalParticleWidth(0.6)
      .onNodeHover(node => {
        document.body.style.cursor = node ? 'pointer' : 'default';
        if (node) {
          tooltip.style.display = 'block';
          tooltip.querySelector('.tt-name').textContent = node.name;
          tooltip.querySelector('.tt-kind').textContent = node.kind;
          tooltip.querySelector('.tt-path').textContent = node.path + ':' + node.line;
        } else {
          tooltip.style.display = 'none';
        }
      })
      .onNodeClick(node => {
        if (!node) return;
        tooltip.style.display = 'block';
        tooltip.querySelector('.tt-name').textContent = node.name;
        tooltip.querySelector('.tt-kind').textContent = node.kind;
        tooltip.querySelector('.tt-path').textContent = node.path + ':' + node.line;
        graph.cameraPosition(
          { x: node.x + 80, y: node.y + 80, z: node.z + 80 },
          { x: node.x, y: node.y, z: node.z },
          1000
        );
      });

    // Track mouse for tooltip positioning
    document.addEventListener('mousemove', e => {
      tooltip.style.left = (e.clientX + 15) + 'px';
      tooltip.style.top = (e.clientY + 15) + 'px';
    });
  })
  .catch(err => {
    loading.textContent = 'Failed to load graph: ' + err;
  });
</script>
</body>
</html>
"##;
