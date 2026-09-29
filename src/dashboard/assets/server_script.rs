//! Server-mode dashboard browser JavaScript.

/// JavaScript for the server-mode shell — all data fetched via API.
/// v6.1: real filters, sort, inline markdown, URL state, expand/collapse,
/// human Bearer login (localStorage) so `/api/*` auth works in a browser.
pub(crate) const DASHBOARD_SERVER_SCRIPT: &str = r#"
(() => {
  const $ = (id) => document.getElementById(id);
  const ui = {
    search: $('ctx-search'), project: $('ctx-project'), agent: $('ctx-agent'),
    kind: $('ctx-kind'), sort: $('ctx-sort'), score: $('ctx-score'),
    scoreLabel: $('ctx-score-label'), summary: $('ctx-summary'), list: $('ctx-list'),
    detailTitle: $('ctx-detail-title'), detailMeta: $('ctx-detail-meta'),
    detailContent: $('ctx-detail-content'), assumptions: $('ctx-assumptions'),
    copyPath: $('ctx-copy-path'), expand: $('ctx-expand'),
    genInfo: $('ctx-gen-info'), statFiles: $('ctx-stat-files'),
    statProjects: $('ctx-stat-projects'), statDays: $('ctx-stat-days'),
    regenerateBtn: $('ctx-regenerate'),
  };

  const hooks = { beforeRender: [], afterRender: [], onSelect: [] };
  const state = {
    query: '', project: '', agent: '', kind: '', sort: 'newest', since: '',
    scoreMin: 0, limit: 350, selectedId: null, rows: [], selectedRecord: null,
    browseRecords: [], mode: 'browse', expanded: false, unit: 'session',
    assumptions: [], indexLoaded: false, corpusTotal: 0, browseRetries: 0,
  };

  const withoutStamp = (line) => {
    if (line.charAt(0) !== '[') return line;
    const end = line.indexOf(']');
    return end > 0 ? line.slice(end + 1).trim() : line;
  };
  const stripAnsi = (value) => String(value || '')
    .replace(/\u001b\[[0-9;?]*[ -/]*[@-~]/g, '')
    .replace(/\u001b\][^\u0007]*(?:\u0007|\u001b\\)/g, '');
  const readableLine = (line) => {
    let text = stripAnsi(withoutStamp(line)).replace(/^(user|assistant):\s*/i, '').replace(/<\/?user_query>/gi, ' ').replace(/\s+/g, ' ').trim();
    if (!text || text.indexOf('<') === 0) return '';
    if (/[\u2500-\u257F]/.test(text)) return '';
    return text.length > 140 ? text.slice(0, 140) + '\u2026' : text;
  };
  const readableName = (record) => {
    const preview = stripAnsi(record.preview || record.excerpt || '');
    const lines = preview.split('\n').map(function(line) { return line.trim(); }).filter(Boolean);
    const spoken = function(prefix) {
      return lines.find(function(line) { return withoutStamp(line).toLowerCase().indexOf(prefix) === 0 && readableLine(line); });
    };
    const chosen = spoken('user:') || spoken('assistant:');
    if (chosen) return readableLine(chosen);
    const first = lines.map(readableLine).find(Boolean);
    if (first) return first;
    const raw = record.file_name || record.file || '';
    const label = (record.label || '').trim();
    if (label && !/\.jsonl?$/i.test(label)) return label;
    if (record.kind === 'session' || /\.jsonl$/i.test(raw)) return (record.agent || 'session') + ' session';
    return raw || '(unnamed)';
  };
  const listPreview = (record) => {
    const text = stripAnsi((record.preview || record.excerpt || '').replace(/\[1970-01-01[^\]]*\]\s*/g, '')).trim();
    if (!text || text.indexOf('<rules>') === 0) return '';
    return text.length > 240 ? text.slice(0, 240) + '\u2026' : text;
  };

  const renderMarkdown = AicxMarkdown.renderMarkdown;

  /* --- browser Bearer auth (human UX; server still owns cascade) -------- */
  const AUTH_KEY = 'aicx_dashboard_token';
  const getToken = () => {
    try { return localStorage.getItem(AUTH_KEY) || ''; } catch (_) { return ''; }
  };
  const clearToken = () => {
    try { localStorage.removeItem(AUTH_KEY); } catch (_) {}
  };
  const saveToken = (tok) => {
    try {
      if (tok) localStorage.setItem(AUTH_KEY, tok);
      else clearToken();
    } catch (_) {}
  };
  const ensureLoginDom = () => {
    if (document.getElementById('aicx-auth-overlay')) return;
    const overlay = document.createElement('div');
    overlay.id = 'aicx-auth-overlay';
    overlay.className = 'aicx-auth-overlay';
    overlay.innerHTML =
      '<div class="aicx-auth-card">' +
      '<h2>AICX Dashboard</h2>' +
      '<p>This server protects <code>/api/*</code> with a Bearer token. Paste the token from <code>~/.aicx/auth-token</code> (or <code>AICX_HTTP_AUTH_TOKEN</code>). It stays in this browser only (localStorage).</p>' +
      '<div class="aicx-auth-err" id="aicx-auth-err" style="display:none"></div>' +
      '<form id="aicx-auth-form">' +
      '<input type="password" id="aicx-auth-tok" placeholder="Bearer token" autocomplete="off" autofocus />' +
      '<button type="submit" id="aicx-auth-btn">Authenticate</button>' +
      '</form></div>';
    document.body.appendChild(overlay);
    document.getElementById('aicx-auth-form').addEventListener('submit', function(e) {
      e.preventDefault();
      const tok = (document.getElementById('aicx-auth-tok').value || '').trim();
      if (!tok) return;
      const err = document.getElementById('aicx-auth-err');
      const btn = document.getElementById('aicx-auth-btn');
      btn.disabled = true;
      if (err) err.style.display = 'none';
      fetch('/api/status', { headers: { 'Authorization': 'Bearer ' + tok } })
        .then(function(r) {
          if (!r.ok) {
            if (err) {
              err.textContent = 'Invalid token (HTTP ' + r.status + ').';
              err.style.display = 'block';
            }
            return;
          }
          saveToken(tok);
          hideLogin();
          loadBrowseData();
        })
        .catch(function(ex) {
          if (err) {
            err.textContent = 'Auth probe failed: ' + ex.message;
            err.style.display = 'block';
          }
        })
        .finally(function() { btn.disabled = false; });
    });
  };
  const hideLogin = () => {
    const el = document.getElementById('aicx-auth-overlay');
    if (el) el.style.display = 'none';
  };
  const showLogin = (msg) => {
    ensureLoginDom();
    const el = document.getElementById('aicx-auth-overlay');
    const err = document.getElementById('aicx-auth-err');
    const tok = document.getElementById('aicx-auth-tok');
    if (err) {
      err.textContent = msg || 'Enter the bearer token from ~/.aicx/auth-token (or AICX_HTTP_AUTH_TOKEN).';
      err.style.display = 'block';
    }
    if (el) el.style.display = 'flex';
    if (tok) { tok.value = ''; setTimeout(function() { tok.focus(); }, 0); }
  };
  const apiFetch = (url, opts) => {
    opts = opts || {};
    const headers = Object.assign({}, opts.headers || {});
    const t = getToken();
    if (t) headers['Authorization'] = 'Bearer ' + t;
    return fetch(url, Object.assign({}, opts, { headers: headers })).then(function(r) {
      if (r.status === 401) {
        clearToken();
        showLogin('Unauthorized. Paste a valid token and try again.');
        const err = new Error('unauthorized');
        err.status = 401;
        throw err;
      }
      return r;
    });
  };

  /* --- URL state --------------------------------------------------------- */
  const pushUrlState = () => {
    const p = new URLSearchParams();
    if (state.query) p.set('q', state.query);
    if (state.project) p.set('project', state.project);
    if (state.agent) p.set('agent', state.agent);
    if (state.kind) p.set('kind', state.kind);
    if (state.sort !== 'newest') p.set('sort', state.sort);
    if (state.since) p.set('since', state.since);
    if (state.scoreMin > 0) p.set('score', String(state.scoreMin));
    const qs = p.toString();
    const url = qs ? '?' + qs : location.pathname;
    history.replaceState(null, '', url);
  };
  const readUrlState = () => {
    const p = new URLSearchParams(location.search);
    if (p.has('q')) { state.query = p.get('q'); ui.search.value = state.query; }
    if (p.has('project')) { state.project = p.get('project'); ui.project.value = state.project; }
    if (p.has('agent')) { state.agent = p.get('agent'); ui.agent.value = state.agent; }
    if (p.has('kind')) { state.kind = p.get('kind'); ui.kind.value = state.kind; }
    if (p.has('sort')) { state.sort = p.get('sort'); ui.sort.value = state.sort; }
    if (p.has('since')) { state.since = p.get('since'); setTimeBtnActive(state.since); }
    if (p.has('score')) { state.scoreMin = parseInt(p.get('score'), 10) || 0; ui.score.value = state.scoreMin; ui.scoreLabel.textContent = state.scoreMin; }
  };

  /* --- helpers ----------------------------------------------------------- */
  const fillSelect = (node, values) => {
    const cur = node.value;
    while (node.options.length > 1) node.remove(1);
    values.forEach((v) => { const o = document.createElement('option'); o.value = v; o.textContent = v; node.appendChild(o); });
    if (cur) node.value = cur;
  };
  const runHooks = (name, value) => {
    const list = hooks[name] || [];
    return list.reduce((acc, fn) => { try { const m = fn(acc, null, state); return m === undefined ? acc : m; } catch (_) { return acc; } }, value);
  };
  const escapeHtml = (text) => { const d = document.createElement('div'); d.appendChild(document.createTextNode(text)); return d.innerHTML; };
  const normalizeText = (text) => {
    const map = {'\u0104':'A','\u0105':'a','\u0106':'C','\u0107':'c','\u0118':'E','\u0119':'e','\u0141':'L','\u0142':'l','\u0143':'N','\u0144':'n','\u00D3':'O','\u00F3':'o','\u015A':'S','\u015B':'s','\u0179':'Z','\u017A':'z','\u017B':'Z','\u017C':'z'};
    return text.replace(/[\u0104\u0105\u0106\u0107\u0118\u0119\u0141\u0142\u0143\u0144\u00D3\u00F3\u015A\u015B\u0179\u017A\u017B\u017C]/g, function(c) { return map[c] || c; }).toLowerCase();
  };
  const escapeRegex = (s) => s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  const highlightTerms = (text, query) => {
    if (!query || !text) return escapeHtml(text || '');
    const terms = query.trim().toLowerCase().split(/\s+/).filter(Boolean);
    if (!terms.length) return escapeHtml(text);
    const kinds = new Array(text.length).fill('');
    const markRange = (start, len, cls, ow) => { const end = Math.min(text.length, start + len); for (let i = start; i < end; i++) { if (ow || !kinds[i]) kinds[i] = cls; } };
    terms.forEach(function(term) { const re = new RegExp(escapeRegex(term), 'gi'); let m; while ((m = re.exec(text)) !== null) { if (!m[0]) break; markRange(m.index, m[0].length, 'hl', true); } });
    const normalizedText = normalizeText(text);
    terms.map(normalizeText).filter(Boolean).forEach(function(term) { let sf = 0; while (sf < normalizedText.length) { const idx = normalizedText.indexOf(term, sf); if (idx === -1) break; markRange(idx, term.length, 'hl-fuzzy', false); sf = idx + Math.max(term.length, 1); } });
    let html = ''; let start = 0;
    while (start < text.length) { const cls = kinds[start]; let end = start + 1; while (end < text.length && kinds[end] === cls) end++; const chunk = escapeHtml(text.slice(start, end)); html += cls ? '<mark class="' + cls + '">' + chunk + '</mark>' : chunk; start = end; }
    return html;
  };

  /* --- detail pane ------------------------------------------------------- */
  const renderDetail = (record, score) => {
    state.selectedRecord = record || null;
    state.expanded = false;
    if (ui.expand) ui.expand.textContent = 'Expand';
    if (!record) {
      ui.detailTitle.textContent = 'No result selected';
      ui.detailMeta.textContent = '';
      ui.detailContent.textContent = 'Open a session.';
      return;
    }
    const title = readableName(record);
    const scoreTxt = typeof score === 'number' && score > 0 ? 'score ' + score + '/100' : '';
    const meta = [record.project, record.agent, record.kind, record.date, scoreTxt].filter(Boolean).join(' \u2022 ');
    ui.detailTitle.innerHTML = highlightTerms(title, state.query);
    ui.detailMeta.innerHTML = highlightTerms(meta, state.query);
    const previewText = stripAnsi(record.preview || record.excerpt || '');
    if (previewText) {
      ui.detailContent.innerHTML = '<div class="md-rendered">' + renderMarkdown(previewText) + '</div>';
    } else {
      ui.detailContent.textContent = '(no preview)';
    }
  };

  const expandDetail = () => {
    const rec = state.selectedRecord;
    if (!rec) return;
    if (state.expanded) {
      renderDetail(rec, 0);
      return;
    }
    ui.detailContent.textContent = 'Loading full content\u2026';
    if (rec.id === undefined || rec.id === null || rec.id === '') {
      state.expanded = true;
      if (ui.expand) ui.expand.textContent = 'Collapse';
      ui.detailContent.innerHTML = '<div class="md-rendered">' + renderMarkdown(rec.excerpt || rec.preview || '') + '</div>';
      return;
    }
    apiFetch('/api/detail?id=' + encodeURIComponent(rec.id))
      .then(function(r) { return r.json(); })
      .then(function(data) {
        if (!data.ok) { ui.detailContent.textContent = 'Failed: ' + (data.error || 'unknown'); return; }
        const content = stripAnsi(data.content || data.detail_text || '');
        state.expanded = true;
        if (ui.expand) ui.expand.textContent = 'Collapse';
        ui.detailContent.innerHTML = '<div class="md-rendered">' + renderMarkdown(content) + '</div>';
      })
      .catch(function(err) {
        if (err && err.status === 401) return;
        ui.detailContent.textContent = 'Load failed: ' + err.message;
      });
  };

  /* --- result list ------------------------------------------------------- */
  const corpusQuiet = () => !state.query && !state.project && !state.agent && !state.kind && !state.since && state.scoreMin === 0;
  const emptyReason = () => {
    if (!corpusQuiet()) return 'No sessions match this search.';
    const note = (state.assumptions || []).find(Boolean);
    if (note) return note;
    if (!state.indexLoaded) return 'No index on this machine. Search only sees what has been scanned.';
    return 'No sessions in this corpus.';
  };
  const mkBadge = (txt) => { const n = document.createElement('span'); n.className = 'badge'; n.innerHTML = highlightTerms(String(txt || ''), state.query); return n; };
  const renderList = (rows) => {
    ui.list.innerHTML = '';
    if (!rows.length) {
      const e = document.createElement('div'); e.className = 'empty'; e.textContent = emptyReason();
      ui.list.appendChild(e); renderDetail(null, 0); return;
    }
    const visible = rows.slice(0, state.limit);
    const idKey = (r) => r.id !== undefined ? r.id : r.path;
    if (!state.selectedId || !visible.some(function(r) { return idKey(r.record) === state.selectedId; })) {
      state.selectedId = idKey(visible[0].record);
    }
    visible.forEach(function(entry) {
      const record = entry.record; const score = entry.score;
      const item = document.createElement('button'); item.type = 'button';
      const rid = idKey(record);
      item.className = 'result-item' + (rid === state.selectedId ? ' active' : '');
      const top = document.createElement('div'); top.className = 'result-top';
      top.appendChild(mkBadge(record.project || 'project'));
      top.appendChild(mkBadge(record.agent || 'agent'));
      top.appendChild(mkBadge(record.kind || 'kind'));
      top.appendChild(mkBadge(record.date || ''));
      if (typeof score === 'number' && score > 0) top.appendChild(mkBadge(score + '/100'));
      const name = document.createElement('div'); name.className = 'result-name';
      const rawName = record.file_name || record.file || '';
      const hideSize = record.kind === 'session' || /\.jsonl$/i.test(rawName);
      const fname = readableName(record) + (!hideSize && record.size_human ? ' \u2022 ' + record.size_human : '');
      name.innerHTML = highlightTerms(fname, state.query);
      item.appendChild(top); item.appendChild(name);
      const previewText = listPreview(record);
      if (previewText) {
        const preview = document.createElement('div'); preview.className = 'result-preview';
        const maxLen = 240; const truncated = previewText.length > maxLen ? previewText.slice(0, maxLen) + '\u2026' : previewText;
        preview.innerHTML = highlightTerms(truncated, state.query);
        item.appendChild(preview);
      }
      const whereText = record.relative_path || record.path || '';
      if (whereText) {
        const where = document.createElement('div'); where.className = 'result-where';
        where.textContent = whereText;
        item.appendChild(where);
      }
      item.addEventListener('click', function() {
        state.selectedId = rid; renderList(state.rows); renderDetail(record, score); runHooks('onSelect', record);
      });
      ui.list.appendChild(item);
    });
    const sel = visible.find(function(r) { return idKey(r.record) === state.selectedId; }) || visible[0];
    if (sel) renderDetail(sel.record, sel.score);
  };

  /* --- browse + search --------------------------------------------------- */
  const applyBrowseFilters = () => {
    state.mode = 'browse';
    let rows = state.browseRecords
      .map(function(r) { return { record: r, score: 0 }; });
    const sortDir = state.sort;
    if (sortDir === 'oldest') rows.sort(function(a, b) { return (a.record.sort_ts || 0) - (b.record.sort_ts || 0); });
    else rows.sort(function(a, b) { return (b.record.sort_ts || 0) - (a.record.sort_ts || 0); });
    rows = runHooks('beforeRender', rows);
    state.rows = rows;
    const unit = state.unit === 'file' ? 'file' : 'session';
    ui.summary.textContent = (!rows.length && corpusQuiet())
      ? emptyReason()
      : rows.length + ' ' + unit + (rows.length === 1 ? '' : 's');
    renderList(rows);
    runHooks('afterRender', rows);
  };

  let searchAbort = null;
  const runSearch = () => {
    state.mode = 'search';
    const q = state.query;
    if (!q) { applyBrowseFilters(); return; }
    if (searchAbort) searchAbort.abort();
    searchAbort = new AbortController();
    ui.summary.textContent = 'Searching\u2026';
    const params = new URLSearchParams({ q: q, limit: '100' });
    if (state.project) params.set('project', state.project);
    if (state.scoreMin > 0) params.set('score', String(state.scoreMin));
    apiFetch('/api/search/semantic?' + params.toString(), { signal: searchAbort.signal })
      .then(function(r) { return r.json(); })
      .then(function(data) {
        if (!data.ok) { ui.summary.textContent = 'Search error: ' + (data.error || 'unknown'); return; }
        let rows = data.results.map(function(r) { return { record: r, score: r.score || 0 }; });
        rows = rows.filter(function(r) {
          if (state.agent && r.record.agent !== state.agent) return false;
          if (state.kind && r.record.kind !== state.kind) return false;
          return true;
        });
        if (state.sort === 'score') rows.sort(function(a, b) { return b.score - a.score; });
        else if (state.sort === 'oldest') rows.sort(function(a, b) { return (a.record.sort_ts || a.record.date || '') < (b.record.sort_ts || b.record.date || '') ? -1 : 1; });
        rows = runHooks('beforeRender', rows);
        state.rows = rows;
        ui.summary.textContent = rows.length + ' result(s) | fuzzy search | scanned: ' + (data.total_scanned || '?');
        renderList(rows);
        runHooks('afterRender', rows);
      })
      .catch(function(err) {
        if (err.name === 'AbortError') return;
        if (err && err.status === 401) return;
        ui.summary.textContent = 'Search failed: ' + err.message;
      });
  };

  const refresh = () => {
    state.query = (ui.search.value || '').trim().toLowerCase();
    state.project = ui.project.value;
    state.agent = ui.agent.value;
    state.kind = ui.kind.value;
    state.sort = ui.sort.value;
    state.scoreMin = parseInt(ui.score.value, 10) || 0;
    pushUrlState();
    if (state.query) { runSearch(); } else { loadBrowseData(); }
  };

  /* --- event wiring ------------------------------------------------------ */
  const DEBOUNCE_MS = 800;
  let debounceTimer = null;
  const liveCheckbox = $('ctx-live');
  const scheduleRefresh = () => { clearTimeout(debounceTimer); debounceTimer = setTimeout(refresh, DEBOUNCE_MS); };
  ui.search.addEventListener('input', function() { if (liveCheckbox.checked) scheduleRefresh(); });
  ui.search.addEventListener('keydown', function(e) { if (e.key === 'Enter') { clearTimeout(debounceTimer); refresh(); } });
  ['input', 'change'].forEach(function(ev) {
    ui.project.addEventListener(ev, refresh);
    ui.agent.addEventListener(ev, refresh);
    ui.kind.addEventListener(ev, refresh);
    ui.sort.addEventListener(ev, refresh);
  });
  liveCheckbox.addEventListener('change', function() { if (liveCheckbox.checked) scheduleRefresh(); });
  ui.score.addEventListener('input', function() { ui.scoreLabel.textContent = ui.score.value; });
  ui.score.addEventListener('change', refresh);
  if (ui.expand) ui.expand.addEventListener('click', expandDetail);
  ui.copyPath.addEventListener('click', async function() {
    const p = state.selectedRecord?.absolute_path || state.selectedRecord?.path || state.selectedRecord?.relative_path || '';
    if (p && navigator.clipboard) { try { await navigator.clipboard.writeText(p); } catch (_) {} }
  });

  /* --- time buttons ------------------------------------------------------ */
  const setTimeBtnActive = (since) => {
    document.querySelectorAll('.time-btn').forEach(function(btn) {
      btn.classList.toggle('active', btn.dataset.since === since);
    });
  };
  document.querySelectorAll('.time-btn').forEach(function(btn) {
    btn.addEventListener('click', function() {
      state.since = btn.dataset.since;
      setTimeBtnActive(state.since);
      refresh();
    });
  });

  /* --- regenerate -------------------------------------------------------- */
  if (ui.regenerateBtn) {
    ui.regenerateBtn.addEventListener('click', function() {
      ui.regenerateBtn.disabled = true; ui.regenerateBtn.textContent = '\u2026';
      apiFetch('/api/regenerate', { method: 'POST', headers: { 'x-ai-contexters-action': 'regenerate' } })
        .then(function(r) { return r.json(); })
        .then(function(data) { if (data.ok) loadBrowseData(); else alert('Regenerate failed: ' + (data.error || 'unknown')); })
        .catch(function(err) {
          if (err && err.status === 401) return;
          alert('Regenerate error: ' + err.message);
        })
        .finally(function() { ui.regenerateBtn.disabled = false; ui.regenerateBtn.textContent = '\u21BB'; });
    });
  }

  /* --- resizable panels -------------------------------------------------- */
  const resizeHandle = $('ctx-resize-handle');
  const layoutEl = $('ctx-layout');
  if (resizeHandle && layoutEl) {
    const SK = 'aicx-split-ratio';
    const saved = localStorage.getItem(SK);
    if (saved) { const r = parseFloat(saved); if (r > 0 && r < 1) layoutEl.style.gridTemplateColumns = r + 'fr 6px ' + (1 - r) + 'fr'; }
    let dragging = false;
    resizeHandle.addEventListener('mousedown', function(e) { e.preventDefault(); dragging = true; resizeHandle.classList.add('dragging'); document.body.style.cursor = 'col-resize'; document.body.style.userSelect = 'none'; });
    document.addEventListener('mousemove', function(e) { if (!dragging) return; const rect = layoutEl.getBoundingClientRect(); const x = e.clientX - rect.left; const total = rect.width - 6; const lw = Math.max(250, Math.min(x, total - 300)); const ratio = lw / total; layoutEl.style.gridTemplateColumns = ratio + 'fr 6px ' + (1 - ratio) + 'fr'; localStorage.setItem(SK, ratio.toFixed(4)); });
    document.addEventListener('mouseup', function() { if (!dragging) return; dragging = false; resizeHandle.classList.remove('dragging'); document.body.style.cursor = ''; document.body.style.userSelect = ''; });
  }

  /* --- load browse data -------------------------------------------------- */
  const showCorpusBusy = () => {
    ui.summary.textContent = 'Still reading the corpus. Retrying\u2026';
    if (ui.genInfo) ui.genInfo.textContent = 'Still reading the corpus';
  };
  const loadBrowseData = () => {
    ui.summary.textContent = 'Loading\u2026';
    const params = new URLSearchParams();
    if (state.project) params.set('project', state.project);
    if (state.agent) params.set('agent', state.agent);
    if (state.kind) params.set('kind', state.kind);
    if (state.sort) params.set('sort', state.sort);
    if (state.since) params.set('since', state.since);
    const qs = params.toString();
    const controller = new AbortController();
    const timer = setTimeout(function() { controller.abort(); }, 8000);
    apiFetch('/api/browse' + (qs ? '?' + qs : ''), { signal: controller.signal })
      .then(function(r) {
        clearTimeout(timer);
        if (r.status === 503) {
          return r.json().then(function(data) {
            const err = new Error((data && data.error) || 'still_reading');
            err.status = 503;
            err.stillReading = data && data.error === 'still_reading';
            throw err;
          });
        }
        if (!r.ok) {
          const err = new Error('HTTP ' + r.status);
          err.status = r.status;
          throw err;
        }
        return r.json();
      })
      .then(function(data) {
        if (!data.ok) {
          if (data.error === 'still_reading') {
            showCorpusBusy();
            setTimeout(loadBrowseData, 2500);
            return;
          }
          ui.summary.textContent = 'Failed: ' + (data.error || 'unknown');
          return;
        }
        state.browseRetries = 0;
        state.browseRecords = data.records || [];
        fillSelect(ui.project, data.projects || []);
        fillSelect(ui.agent, data.agents || []);
        fillSelect(ui.kind, data.kinds || []);
        const s = data.stats || {};
        state.assumptions = data.assumptions || [];
        state.indexLoaded = !!s.index_loaded;
        state.corpusTotal = s.total_files || 0;
        state.unit = s.search_backend === 'catalog-live-source' ? 'session' : 'file';
        const quietEmpty = state.corpusTotal === 0 && !s.index_loaded && !s.state_loaded;
        ui.statFiles.textContent = quietEmpty ? '\u2014' : String(s.total_files || 0);
        ui.statProjects.textContent = quietEmpty ? '\u2014' : String(s.total_projects || 0);
        ui.statDays.textContent = quietEmpty ? '\u2014' : String(s.total_days || 0);
        const unitLabel = document.getElementById('ctx-stat-unit');
        if (unitLabel) unitLabel.textContent = (state.unit === 'session' || quietEmpty) ? 'sessions' : 'files';
        const cardSessions = document.getElementById('ctx-card-sessions');
        if (cardSessions) cardSessions.textContent = ui.statFiles.textContent;
        const cardProjects = document.getElementById('ctx-card-projects');
        if (cardProjects) cardProjects.textContent = ui.statProjects.textContent;
        const indexState = document.getElementById('ctx-index-state');
        if (indexState) indexState.textContent = s.index_loaded ? 'ready' : (quietEmpty ? 'not loaded' : 'partial');
        const scope = document.getElementById('ctx-scope');
        if (scope) {
          const projects = data.projects || [];
          scope.textContent = projects.length === 1 ? projects[0] : (projects.length ? (projects.length + ' projects') : 'This machine');
        }
        ui.genInfo.textContent = 'Generated ' + (data.generated_at || '?');
        ui.assumptions.innerHTML = '';
        (data.assumptions || []).forEach(function(a) { const li = document.createElement('li'); li.textContent = a; ui.assumptions.appendChild(li); });
        applyBrowseFilters();
      })
      .catch(function(err) {
        clearTimeout(timer);
        if (err && err.status === 401) return;
        const busy = (err && err.stillReading)
          || (err && err.name === 'AbortError')
          || (err && !err.status && /fetch|abort/i.test(String(err.message || '')));
        if (busy) {
          state.browseRetries = (state.browseRetries || 0) + 1;
          showCorpusBusy();
          if (state.browseRetries < 8) {
            setTimeout(loadBrowseData, 2500);
            return;
          }
        }
        ui.summary.textContent = 'Load failed: ' + ((err && err.message) || 'unknown');
      });
  };

  const consumeQueryToken = () => {
    try {
      const p = new URLSearchParams(location.search);
      if (!p.has('token')) return;
      const t = (p.get('token') || '').trim();
      if (t) saveToken(t);
      p.delete('token');
      const qs = p.toString();
      history.replaceState(null, '', qs ? (location.pathname + '?' + qs) : location.pathname);
    } catch (_) {}
  };

  const studio = () => {
    const onboard = $('ctx-onboarding');
    const dismiss = $('ctx-onboarding-dismiss');
    try {
      if (localStorage.getItem('aicx_onboarding_dismissed') === '1' && onboard) onboard.open = false;
    } catch (_) {}
    if (dismiss) dismiss.addEventListener('click', () => {
      if (onboard) onboard.open = false;
      try { localStorage.setItem('aicx_onboarding_dismissed', '1'); } catch (_) {}
    });
    const survey = $('ctx-onboarding-phrases');
    const surveySave = $('ctx-onboarding-save');
    const surveyStatus = $('ctx-onboarding-status');
    if (surveySave && survey) surveySave.addEventListener('click', () => {
      const phrases = survey.value.split(/\n/).map((line) => line.trim()).filter(Boolean);
      apiFetch('/api/onboarding', {
        method: 'POST',
        body: JSON.stringify({ phrases: phrases }),
        headers: { 'content-type': 'application/json', 'x-ai-contexters-action': 'regenerate' }
      })
        .then((r) => r.json())
        .then((body) => {
          if (!surveyStatus) return;
          if (!body.ok) {
            surveyStatus.textContent = body.detail || 'Not saved.';
            return;
          }
          const service = body.service ? ' Background service: ' + body.service + '.' : '';
          surveyStatus.textContent = 'Saved. Search will use these phrases.' + service;
        })
        .catch(() => { if (surveyStatus) surveyStatus.textContent = 'Not saved.'; });
    });
    const phrases = $('ctx-phrases');
    const phraseStatus = $('ctx-phrases-status');
    const phraseSave = $('ctx-phrases-save');
    if (phrases) {
      apiFetch('/api/phrases').then((r) => r.text()).then((text) => { phrases.value = text; }).catch(() => {});
    }
    if (phraseSave && phrases) phraseSave.addEventListener('click', () => {
      apiFetch('/api/phrases', { method: 'PUT', body: phrases.value, headers: { 'content-type': 'text/plain', 'x-ai-contexters-action': 'regenerate' } })
        .then((r) => r.json())
        .then((body) => { if (phraseStatus) phraseStatus.textContent = body.ok ? 'Saved.' : (body.detail || 'Not saved.'); })
        .catch(() => { if (phraseStatus) phraseStatus.textContent = 'Not saved.'; });
    });
    const indexBtn = $('ctx-index');
    const indexStatus = $('ctx-index-status');
    if (indexBtn) indexBtn.addEventListener('click', () => {
      indexBtn.disabled = true;
      if (indexStatus) indexStatus.textContent = 'Indexing…';
      apiFetch('/api/index', { method: 'POST', headers: { 'x-ai-contexters-action': 'regenerate' } })
        .then((r) => { if (indexStatus) indexStatus.textContent = r.status === 202 ? 'Index started.' : 'Index was refused.'; })
        .catch(() => { if (indexStatus) indexStatus.textContent = 'Index was refused.'; })
        .finally(() => { indexBtn.disabled = false; });
    });
  };

  const navSessions = $('ctx-nav-sessions');
  const navSetup = $('ctx-nav-setup');
  const markNav = (active) => {
    document.querySelectorAll('.rail-nav-item').forEach(function(btn) { btn.classList.toggle('active', btn === active); });
  };
  if (navSessions) navSessions.addEventListener('click', function() {
    markNav(navSessions);
    if (ui.list) ui.list.scrollTop = 0;
    if (ui.search) ui.search.focus();
  });
  if (navSetup) navSetup.addEventListener('click', function() {
    markNav(navSetup);
    const onboard = $('ctx-onboarding');
    if (onboard) onboard.open = true;
  });

  const boot = () => {
    readUrlState();
    consumeQueryToken();
    studio();
    const headers = {};
    const t = getToken();
    if (t) headers['Authorization'] = 'Bearer ' + t;
    // /api/status is Bearer-gated when auth is on; public when --no-require-auth.
    fetch('/api/status', { headers: headers })
      .then(function(r) {
        if (r.status === 401) {
          showLogin();
          return null;
        }
        return r.json();
      })
      .then(function(data) {
        if (!data) return;
        const host = location.hostname;
        if (host === '127.0.0.1' || host === 'localhost' || host === '::1') {
          const access = document.getElementById('ctx-access');
          if (access) access.textContent = 'This machine. No sign-in.';
        }
        const onboard = $('ctx-onboarding');
        if (onboard && data.survey_required) onboard.open = true;
        if (data.rebuilding) showCorpusBusy();
        loadBrowseData();
      })
      .catch(function() { loadBrowseData(); });
  };

  window.AIContextersDashboard = {
    version: '6.1.0-pwa-auth',
    state: state,
    registerHook: function(name, fn) { if (!hooks[name] || typeof fn !== 'function') return false; hooks[name].push(fn); return true; },
    refresh: refresh,
    reload: loadBrowseData,
    clearAuth: function() { clearToken(); showLogin('Token cleared.'); },
  };

  boot();
})();
"#;
