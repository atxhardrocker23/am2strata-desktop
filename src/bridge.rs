use std::collections::BTreeMap;

pub fn splash_html(model_name: &str) -> String {
    let escaped = html_escape(model_name);
    format!(
        r#"<!doctype html>
<html lang="en" data-theme="dark">
<head>
<meta charset="utf-8">
<title>am2Strata — Starting</title>
<style>
  :root {{
    --bg: #0f1318;
    --surface: #171c23;
    --border: rgba(255,255,255,0.09);
    --ink: #e8edf2;
    --muted: #8b95a5;
    --accent: #3DD68C;
  }}
  * {{ box-sizing: border-box; }}
  html, body {{
    margin: 0; height: 100%; background: var(--bg); color: var(--ink);
    font-family: "Segoe UI", system-ui, -apple-system, sans-serif;
    display: flex; align-items: center; justify-content: center;
  }}
  .card {{
    width: min(560px, 92vw);
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 16px;
    padding: 28px;
    box-shadow: 0 24px 60px rgba(0,0,0,0.45);
  }}
  .brand {{
    display: flex; align-items: center; gap: 12px; margin-bottom: 14px;
    font-size: 20px; font-weight: 600;
  }}
  .logo {{
    width: 34px; height: 34px; border-radius: 10px;
    background: #1a2420; border: 1px solid rgba(61,214,140,0.35);
    display: grid; place-items: center; color: var(--accent); font-weight: 700;
  }}
  .sub {{ color: var(--muted); font-size: 14px; margin: 0 0 18px; line-height: 1.5; }}
  .bar {{
    height: 4px; background: rgba(255,255,255,0.08); border-radius: 999px;
    overflow: hidden; margin-bottom: 16px;
  }}
  .bar > i {{
    display: block; height: 100%; width: 38%; background: var(--accent);
    border-radius: 999px; animation: slide 1.4s ease-in-out infinite;
  }}
  @keyframes slide {{
    0% {{ transform: translateX(-110%); }}
    100% {{ transform: translateX(300%); }}
  }}
  .logs {{
    background: #0b0e12; border: 1px solid var(--border); border-radius: 10px;
    padding: 12px; font-family: "Cascadia Code", Consolas, monospace; font-size: 12px;
    color: #9fb0c3; max-height: 170px; overflow-y: auto; white-space: pre-wrap;
    line-height: 1.45;
  }}
  .actions {{ margin-top: 14px; display: flex; justify-content: flex-end; gap: 8px; }}
  button {{
    background: rgba(255,255,255,0.07); color: var(--ink); border: 1px solid var(--border);
    border-radius: 8px; padding: 8px 14px; font-size: 13px; cursor: pointer;
  }}
  button:hover {{ background: rgba(255,255,255,0.12); }}
</style>
</head>
<body>
  <div class="card">
    <div class="brand">
      <div class="logo">a2</div>
      <div>am<b>2</b>Strata Desktop</div>
    </div>
    <p class="sub" id="status-line">Starting local AI engine and loading <b>{escaped}</b> into RTX 3060 VRAM&hellip;</p>
    <div class="bar"><i></i></div>
    <div class="logs" id="boot-logs">Initializing am2strata.exe on 127.0.0.1:8088…</div>
    <div class="actions" id="err-actions" style="display:none">
      <button onclick="window.ipc.postMessage(JSON.stringify({{type:'retry_start'}}))">Retry Launch</button>
    </div>
  </div>
  <script>
    window.__setSplashLogs = (lines, msg, failed) => {{
      if (msg) document.getElementById('status-line').textContent = msg;
      if (lines && lines.length) {{
        const el = document.getElementById('boot-logs');
        el.textContent = lines.join('\n');
        el.scrollTop = el.scrollHeight;
      }}
      if (failed) document.getElementById('err-actions').style.display = 'flex';
    }};
  </script>
</body>
</html>"#
    )
}

pub fn build_init_script(
    initial_storage: &BTreeMap<String, String>,
    initial_telemetry: &crate::telemetry::TelemetrySnapshot,
) -> String {
    let storage_json = serde_json::to_string(initial_storage).unwrap_or_else(|_| "{}".to_string());
    let telemetry_json = serde_json::to_string(initial_telemetry).unwrap_or_else(|_| "null".to_string());
    format!(
        r##"(() => {{
  if (window.__am2DesktopBridgeLoaded) return;
  window.__am2DesktopBridgeLoaded = true;

  // 1. Hydrate localStorage from G:\Strata-3060\memory\desktop-chats.json if missing locally
  try {{
    const diskData = {storage_json};
    for (const [k, v] of Object.entries(diskData)) {{
      if (!k.startsWith("strata.")) continue;
      const cur = localStorage.getItem(k);
      if (cur === null || (k === "strata.chats" && (!cur || cur === "[]") && v && v !== "[]")) {{
        localStorage.setItem(k, v);
      }}
    }}
    if (!localStorage.getItem("am2.web_enabled_v1")) {{
      let s = {{}};
      try {{ s = JSON.parse(localStorage.getItem("strata.sampling") || "{{}}") || {{}}; }} catch (e) {{}}
      s.web = true;
      s.browser = true;
      localStorage.setItem("strata.sampling", JSON.stringify(s));
      localStorage.setItem("am2.web_enabled_v1", "1");
    }}
  }} catch (e) {{}}

  // 2. Mirror strata.* localStorage updates to disk via Rust IPC
  const postIpc = (payload) => {{
    try {{
      if (window.ipc && typeof window.ipc.postMessage === "function") {{
        window.ipc.postMessage(JSON.stringify(payload));
      }}
    }} catch (e) {{}}
  }};

  const origSetItem = Storage.prototype.setItem;
  Storage.prototype.setItem = function(key, value) {{
    origSetItem.apply(this, arguments);
    if (this === window.localStorage && typeof key === "string" && key.startsWith("strata.")) {{
      postIpc({{ type: "sync_storage", key, value: String(value) }});
    }}
  }};

  const origRemoveItem = Storage.prototype.removeItem;
  Storage.prototype.removeItem = function(key) {{
    origRemoveItem.apply(this, arguments);
    if (this === window.localStorage && typeof key === "string" && key.startsWith("strata.")) {{
      postIpc({{ type: "sync_storage", key, value: null }});
    }}
  }};

  // 3. Initial full sync back to disk once page loads so existing chats are captured
  window.addEventListener("DOMContentLoaded", () => {{
    try {{
      for (let i = 0; i < localStorage.length; i++) {{
        const k = localStorage.key(i);
        if (k && k.startsWith("strata.")) {{
          postIpc({{ type: "sync_storage", key: k, value: localStorage.getItem(k) }});
        }}
      }}
    }} catch (e) {{}}

    // Ensure microphone button (#mic-btn) stays visible in the composer bar
    const ensureMicVisible = () => {{
      const mic = document.getElementById("mic-btn");
      if (mic) {{
        if (mic.hidden) mic.hidden = false;
        if (!mic.dataset.am2MicObserved) {{
          mic.dataset.am2MicObserved = "1";
          const obs = new MutationObserver(() => {{
            if (mic.hidden) mic.hidden = false;
          }});
          obs.observe(mic, {{ attributes: true, attributeFilter: ["hidden"] }});
        }}
      }}
    }};
    ensureMicVisible();

    // Improve drag-and-drop visual feedback across the entire chat panel (pictures, PDFs, videos, audio, code)
    const composer = document.getElementById("composer");
    const viewChat = document.getElementById("view-chat");
    if (viewChat && composer) {{
      viewChat.addEventListener("dragover", (e) => {{
        const types = e.dataTransfer ? Array.from(e.dataTransfer.types || []) : [];
        if (types.includes("Files")) {{
          e.preventDefault();
          composer.classList.add("dragging");
        }}
      }});
      viewChat.addEventListener("dragleave", (e) => {{
        if (!viewChat.contains(e.relatedTarget)) composer.classList.remove("dragging");
      }});
      viewChat.addEventListener("drop", (e) => {{
        composer.classList.remove("dragging");
        if (!e.defaultPrevented && e.dataTransfer && e.dataTransfer.files && e.dataTransfer.files.length) {{
          e.preventDefault();
          if (typeof window.addFiles === "function") {{
            window.addFiles(e.dataTransfer.files);
            const inp = document.getElementById("input");
            if (inp) inp.focus();
          }}
        }}
      }});
    }}

    // Prevent dropping files outside #view-chat from accidentally navigating the WebView to file:///
    window.addEventListener("dragover", (e) => {{
      const types = e.dataTransfer ? Array.from(e.dataTransfer.types || []) : [];
      if (types.includes("Files")) e.preventDefault();
    }}, false);
    window.addEventListener("drop", (e) => {{
      if (!e.defaultPrevented && e.dataTransfer && e.dataTransfer.files && e.dataTransfer.files.length) {{
        e.preventDefault();
        if (typeof window.showTab === "function") window.showTab("chat");
        if (typeof window.addFiles === "function") {{
          window.addFiles(e.dataTransfer.files);
          const inp = document.getElementById("input");
          if (inp) inp.focus();
        }}
      }}
    }}, false);
  }});

  // 4. Win32 system telemetry fallback for /metrics, /api/system, and /v1/live WebSocket
  window.__am2Telemetry = {telemetry_json};
  window.__setDesktopTelemetry = (snap) => {{
    if (snap && typeof snap === "object") window.__am2Telemetry = snap;
  }};
  if (document.readyState === "loading") {{
    document.addEventListener("DOMContentLoaded", () => postIpc({{ type: "request_telemetry" }}), {{ once: true }});
  }} else {{
    postIpc({{ type: "request_telemetry" }});
  }}

  const allNull = (arr) => !Array.isArray(arr) || arr.length === 0 || arr.every((v) => v === null || v === undefined);
  const patchMetricsObj = (data, t) => {{
    if (!t || !data || typeof data !== "object") return;
    data.hardware = data.hardware || {{}};
    if (data.hardware.cpu == null && t.cpu_pct != null) data.hardware.cpu = t.cpu_pct;
    if (data.hardware.ram_used == null && t.ram_total > 0) data.hardware.ram_used = t.ram_used;
    if (data.hardware.ram_total == null && t.ram_total > 0) data.hardware.ram_total = t.ram_total;
    if (data.hardware.disk_read_mb == null) data.hardware.disk_read_mb = t.disk_read_mb;
    if (data.hardware.disk_write_mb == null) data.hardware.disk_write_mb = t.disk_write_mb;
    data.history = data.history || {{}};
    if (allNull(data.history.cpu) && Array.isArray(t.hist_cpu)) data.history.cpu = t.hist_cpu;
    if (allNull(data.history.ram_used) && Array.isArray(t.hist_ram_used)) data.history.ram_used = t.hist_ram_used;
    if (allNull(data.history.disk_read_mb) && Array.isArray(t.hist_disk_read_mb)) data.history.disk_read_mb = t.hist_disk_read_mb;
    data.hardware_static = data.hardware_static || {{}};
    if (data.hardware_static.cpu_name == null && t.cpu_name) data.hardware_static.cpu_name = t.cpu_name;
    if (data.hardware_static.cores == null && t.cores != null) data.hardware_static.cores = t.cores;
    if (data.hardware_static.threads == null && t.threads) data.hardware_static.threads = t.threads;
    if (data.hardware_static.psutil == null) data.hardware_static.psutil = true;
  }};
  const patchSystemObj = (data, t) => {{
    if (!t || !data || typeof data !== "object") return;
    data.cpu = data.cpu || {{}};
    if (data.cpu.name == null && t.cpu_name) data.cpu.name = t.cpu_name;
    if (data.cpu.cores == null && t.cores != null) data.cpu.cores = t.cores;
    if (data.cpu.threads == null && t.threads) data.cpu.threads = t.threads;
    if (data.cpu.util_pct == null && t.cpu_pct != null) data.cpu.util_pct = t.cpu_pct;
    data.memory = data.memory || {{}};
    if (data.memory.used_mib == null && t.ram_total > 0) data.memory.used_mib = Math.round(t.ram_used / 1048576);
    if (data.memory.total_mib == null && t.ram_total > 0) data.memory.total_mib = Math.round(t.ram_total / 1048576);
  }};

  const OrigWebSocket = window.WebSocket;
  if (OrigWebSocket) {{
    window.WebSocket = function(url, protocols) {{
      const ws = protocols !== undefined ? new OrigWebSocket(url, protocols) : new OrigWebSocket(url);
      const uStr = String(url || "");
      if (uStr.includes("/v1/live")) {{
        let userOnMessage = null;
        const patchEvent = (ev) => {{
          try {{
            if (typeof ev.data === "string" && window.__am2Telemetry) {{
              const m = JSON.parse(ev.data);
              if (m && m.type === "update" && m.topic === "metrics" && m.data) {{
                patchMetricsObj(m.data, window.__am2Telemetry);
                return new MessageEvent("message", {{
                  data: JSON.stringify(m),
                  origin: ev.origin,
                  lastEventId: ev.lastEventId,
                }});
              }}
            }}
          }} catch (e) {{}}
          return ev;
        }};
        ws.addEventListener("message", (ev) => {{
          if (typeof userOnMessage === "function") userOnMessage.call(ws, patchEvent(ev));
        }});
        Object.defineProperty(ws, "onmessage", {{
          configurable: true,
          get() {{ return userOnMessage; }},
          set(fn) {{ userOnMessage = typeof fn === "function" ? fn : null; }},
        }});
      }}
      return ws;
    }};
    window.WebSocket.prototype = OrigWebSocket.prototype;
    window.WebSocket.CONNECTING = OrigWebSocket.CONNECTING;
    window.WebSocket.OPEN = OrigWebSocket.OPEN;
    window.WebSocket.CLOSING = OrigWebSocket.CLOSING;
    window.WebSocket.CLOSED = OrigWebSocket.CLOSED;
  }}

  const origFetch = window.fetch ? window.fetch.bind(window) : null;
  if (origFetch) {{
    window.fetch = async function(input, init) {{
      let nextInit = init;
      try {{
        const rawUrl = typeof input === "string" ? input : (input && input.url ? input.url : "");
        const u = new URL(rawUrl, window.location.origin);
        if (u.origin === window.location.origin && u.pathname === "/v1/chat/completions" && init && typeof init.body === "string") {{
          const parsed = JSON.parse(init.body);
          if (parsed && typeof parsed === "object" && parsed.strata_web === undefined) {{
            parsed.strata_web = true;
            nextInit = Object.assign({{}}, init, {{ body: JSON.stringify(parsed) }});
          }}
        }}
      }} catch (e) {{}}
      const res = await origFetch(input, nextInit);
      try {{
        const rawUrl = typeof input === "string" ? input : (input && input.url ? input.url : "");
        const u = new URL(rawUrl, window.location.origin);
        if (u.origin !== window.location.origin) return res;
        const path = u.pathname;
        const t = window.__am2Telemetry;
        if (!t || !res.ok) return res;
        if (path === "/metrics" || path === "/api/system") {{
          const data = await res.clone().json();
          if (path === "/metrics") patchMetricsObj(data, t);
          else if (path === "/api/system") patchSystemObj(data, t);
          const headers = new Headers(res.headers);
          headers.set("content-type", "application/json");
          headers.delete("content-length");
          return new Response(JSON.stringify(data), {{
            status: res.status,
            statusText: res.statusText,
            headers,
          }});
        }}
      }} catch (e) {{}}
      return res;
    }};
  }}

  // 5. Desktop keyboard shortcuts & GitHub Updates modal helper
  window.addEventListener("keydown", (e) => {{
    if (e.ctrlKey && e.shiftKey && e.key.toLowerCase() === "u") {{
      e.preventDefault();
      window.__showUpdatesModal("Checking GitHub for updates…");
      postIpc({{ type: "check_updates" }});
    }}
  }});

  window.__desktopToast = (kind, title, text, openPath) => {{
    if (typeof window.toast === "function") {{
      const action = openPath ? {{ label: "Show in folder", run: () => postIpc({{ type: "open_path", path: openPath }}) }} : null;
      window.toast(kind || "info", title || "am2Strata Desktop", text || "", 6000, action);
    }}
  }};

  window.__showUpdatesModal = (content) => {{
    let modal = document.getElementById("am2-desktop-updates-modal");
    if (!modal) {{
      modal = document.createElement("div");
      modal.id = "am2-desktop-updates-modal";
      modal.style.cssText = "position:fixed;inset:0;z-index:9999;background:rgba(0,0,0,0.65);display:flex;align-items:center;justify-content:center;padding:24px;";
      modal.innerHTML = `
        <div style="width:min(780px,95vw);max-height:82vh;display:flex;flex-direction:column;background:var(--st-surface,#171c23);color:var(--st-ink,#e8edf2);border:1px solid var(--st-line,rgba(255,255,255,0.12));border-radius:14px;box-shadow:0 24px 64px rgba(0,0,0,0.55);overflow:hidden;">
          <div style="display:flex;align-items:center;justify-content:space-between;padding:14px 18px;border-bottom:1px solid var(--st-line-soft,rgba(255,255,255,0.08));font-weight:600;">
            <span>PolyStrata &amp; am2Strata GitHub Updates</span>
            <div style="display:flex;gap:8px;">
              <button type="button" id="am2-upd-refresh" class="st-btn st-btn--secondary" style="height:30px;padding:0 12px;">Refresh</button>
              <button type="button" id="am2-upd-close" class="st-btn st-btn--secondary" style="height:30px;padding:0 12px;">Close</button>
            </div>
          </div>
          <pre id="am2-upd-body" style="margin:0;padding:16px 18px;overflow:auto;font-family:var(--st-mono,Consolas,monospace);font-size:12.5px;line-height:1.55;white-space:pre-wrap;"></pre>
        </div>`;
      document.body.appendChild(modal);
      modal.querySelector("#am2-upd-close").onclick = () => modal.remove();
      modal.querySelector("#am2-upd-refresh").onclick = () => {{
        modal.querySelector("#am2-upd-body").textContent = "Checking GitHub for updates…";
        postIpc({{ type: "check_updates" }});
      }};
      modal.addEventListener("click", (ev) => {{ if (ev.target === modal) modal.remove(); }});
    }}
    modal.querySelector("#am2-upd-body").textContent = content || "";
  }};
}})();
"##
    )
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
