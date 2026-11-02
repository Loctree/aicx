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
    browseRecords: [], mode: 'browse', view: 'sessions', intentHours: 720, expanded: false, unit: 'session',
    assumptions: [], indexLoaded: false, corpusTotal: 0, browseRetries: 0,
    scanStatus: 'unknown',
  };

  const applyStatus = (data) => {
    if (!data) return;
    if (data.scan_status) state.scanStatus = data.scan_status;
    if (typeof data.survey_required === 'boolean') {
      setOnboardingReadiness(data.survey_required);
      if (data.survey_required === true && !onboarding.autoOpened) {
        onboarding.autoOpened = true;
        showSetup();
      }
    }
    if (data.scan_status === 'not_scanned') {
      ui.statFiles.textContent = '\u2014';
      ui.statProjects.textContent = '\u2014';
      ui.statDays.textContent = '\u2014';
      if (ui.genInfo) ui.genInfo.textContent = 'Not scanned yet';
    }
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
  const resultFormat = window.AICX_RESULT_FORMAT || {};
  const listSnippetChars = Number(resultFormat.listSnippetChars) || 0;
  const listPreview = (record) => {
    const text = stripAnsi((record.preview || record.excerpt || '').replace(/\[1970-01-01[^\]]*\]\s*/g, '')).trim();
    if (!text || text.indexOf('<rules>') === 0) return '';
    if (listSnippetChars > 0 && text.length > listSnippetChars) return text.slice(0, listSnippetChars) + '\u2026';
    return text;
  };
  const resultHeader = (record, score) => {
    const scored = typeof score === 'number' && score > 0;
    if (record.header && !scored) return record.header;
    const parts = [record.project, record.agent, record.kind, record.date].filter(Boolean);
    if (scored) parts.push(String(score));
    const session = record.session_id ? String(record.session_id).slice(0, 8) : '';
    if (session) parts.push(session);
    return parts.join(' \u00b7 ');
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
            return null;
          }
          return r.json();
        })
        .then(function(data) {
          if (!data) return;
          saveToken(tok);
          hideLogin();
          applyStatus(data);
          if (state.view === 'setup') showSetup();
          else loadBrowseData();
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
    const meta = resultHeader(record, score);
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
    if (rec.surface === 'intent') {
      const params = new URLSearchParams();
      params.set('full', '1');
      params.set('hours', String(state.intentHours));
      params.set('limit', '20');
      if (state.project) params.set('project', state.project);
      if (rec.session_id) params.set('session_id', rec.session_id);
      if (rec.source_chunk) params.set('source_chunk', rec.source_chunk);
      apiFetch('/api/intents?' + params.toString())
        .then(function(r) { return r.json(); })
        .then(function(data) {
          const match = (data.items || []).find(function(item) {
            return item.session_id === rec.session_id && item.source_chunk === rec.source_chunk;
          }) || (data.items || [])[0];
          const content = stripAnsi((match && match.body) || rec.body || '');
          state.expanded = true;
          if (ui.expand) ui.expand.textContent = 'Collapse';
          ui.detailContent.innerHTML = '<div class="md-rendered">' + renderMarkdown(content) + '</div>';
        })
        .catch(function(err) {
          if (err && err.status === 401) return;
          ui.detailContent.textContent = 'Load failed: ' + err.message;
        });
      return;
    }
    if ((rec.id === undefined || rec.id === null || rec.id === '') && rec.detail_text) {
      state.expanded = true;
      if (ui.expand) ui.expand.textContent = 'Collapse';
      ui.detailContent.innerHTML = '<div class="md-rendered">' + renderMarkdown(stripAnsi(rec.detail_text)) + '</div>';
      return;
    }
    if (rec.id === undefined || rec.id === null || rec.id === '') {
      state.expanded = true;
      if (ui.expand) ui.expand.textContent = 'Collapse';
      ui.detailContent.innerHTML = '<div class="md-rendered">' + renderMarkdown(rec.excerpt || rec.preview || '') + '</div>';
      return;
    }
    apiFetch('/api/detail?full=1&id=' + encodeURIComponent(rec.id))
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
    const idKey = (r) => {
      if (r.surface === 'intent') return String(r.session_id || '') + '|' + String(r.source_chunk || '');
      if (r.id !== undefined && r.id !== null && r.id !== '') return r.id;
      return r.path || r.session_id || '';
    };
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
      const hideSize = record.kind === 'session' || record.surface === 'intent' || /\.jsonl$/i.test(rawName);
      const fname = resultHeader(record, score) || (readableName(record) + (!hideSize && record.size_human ? ' \u2022 ' + record.size_human : ''));
      name.innerHTML = highlightTerms(fname, state.query);
      item.appendChild(top); item.appendChild(name);
      const previewText = listPreview(record);
      if (previewText) {
        const preview = document.createElement('div'); preview.className = 'result-preview';
        const truncated = previewText;
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
    const shown = Math.min(rows.length, state.limit);
    ui.summary.textContent = (!rows.length && corpusQuiet())
      ? emptyReason()
      : shown + ' / ' + rows.length + ' ' + unit + (rows.length === 1 ? '' : 's');
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
        ui.summary.textContent = Math.min(rows.length, state.limit) + ' / ' + rows.length + ' results | scanned ' + (data.total_scanned || '?');
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
    if (state.view === 'setup' || state.view === 'continuity') return;
    state.query = (ui.search.value || '').trim().toLowerCase();
    state.project = ui.project.value;
    state.agent = ui.agent.value;
    state.kind = ui.kind.value;
    state.sort = ui.sort.value;
    state.scoreMin = parseInt(ui.score.value, 10) || 0;
    pushUrlState();
    if (state.view === 'intents') { loadIntents(); return; }
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
  const intentHoursFor = (since) => {
    if (since === '1h') return 1;
    if (since === '4h') return 4;
    if (since === '24h') return 24;
    if (since === '7d') return 168;
    if (since === '30d') return 720;
    return 0;
  };
  document.querySelectorAll('.time-btn').forEach(function(btn) {
    btn.addEventListener('click', function() {
      if (state.view === 'intents') {
        state.intentHours = intentHoursFor(btn.dataset.since);
        setTimeBtnActive(btn.dataset.since);
      } else {
        state.since = btn.dataset.since;
        setTimeBtnActive(state.since);
      }
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
    const deferredScan = state.scanStatus === 'not_scanned';
    if (deferredScan) showCorpusBusy();
    else ui.summary.textContent = 'Loading\u2026';
    const params = new URLSearchParams();
    if (state.project) params.set('project', state.project);
    if (state.agent) params.set('agent', state.agent);
    if (state.kind) params.set('kind', state.kind);
    if (state.sort) params.set('sort', state.sort);
    if (state.since) params.set('since', state.since);
    const qs = params.toString();
    const controller = deferredScan ? null : new AbortController();
    const timer = controller ? setTimeout(function() { controller.abort(); }, 8000) : null;
    const requestOptions = controller ? { signal: controller.signal } : {};
    apiFetch('/api/browse' + (qs ? '?' + qs : ''), requestOptions)
      .then(function(r) {
        if (timer) clearTimeout(timer);
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
        state.scanStatus = 'ready';
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

  /* --- full-stage first-run onboarding ----------------------------------- */
  const onboarding = {
    data: null, hydrated: false, autoOpened: false, busy: false,
    building: false, finishing: false, pollTimer: null, loading: null,
    suggestions: null, discovering: null, editedFields: new Set(),
  };
  const phraseFields = {
    intent_keywords: 'ctx-ob-intent-keywords', task_action_heads: 'ctx-ob-task-heads',
    decision_policy: 'ctx-ob-decision-policy', requirement_markers: 'ctx-ob-requirement-markers',
    question_markers: 'ctx-ob-question-markers',
  };
  const obText = (id, value) => { const node = $(id); if (node) node.textContent = value; };
  const obBadge = (id, label, tone) => {
    const node = $(id);
    if (node) { node.textContent = label; node.dataset.tone = tone || ''; }
  };
  const setOnboardingReadiness = (required) => {
    obBadge('ctx-ob-readiness', required ? 'Setup needed' : 'Configured', required ? 'attention' : 'ready');
  };
  const onboardingJson = async (url, payload) => {
    const opts = payload === undefined ? {} : {
      method: 'POST', body: JSON.stringify(payload),
      headers: { 'content-type': 'application/json', 'x-ai-contexters-action': 'regenerate' },
    };
    const response = await apiFetch(url, opts);
    const body = await response.json().catch(() => ({}));
    if (!response.ok || body.ok === false) throw new Error(body.detail || body.error || ('HTTP ' + response.status));
    if (url === '/api/onboarding' && body.ok !== true) throw new Error('Setup save was not confirmed by the server');
    return body;
  };
  // Some markers deliberately end in a space. Preserve the saved dictionary.
  const phraseLines = (id) => ($(id).value || '').split(/\r?\n/).filter((line) => line.trim());
  const personalPhrases = () => {
    const payload = {};
    Object.keys(phraseFields).forEach((key) => { payload[key] = phraseLines(phraseFields[key]); });
    const quick = $('ctx-onboarding-phrases');
    payload.phrases = quick ? phraseLines('ctx-onboarding-phrases') : [];
    return payload;
  };
  const updateOnboardingButtons = () => {
    ['ctx-ob-complete-btn', 'ctx-ob-save-phrases'].forEach((id) => {
      const button = $(id);
      if (button) button.disabled = !onboarding.data || onboarding.busy || onboarding.building;
    });
    const quick = $('ctx-onboarding-save');
    if (quick) quick.disabled = onboarding.busy || onboarding.building;
    const index = $('ctx-index');
    if (index) index.disabled = onboarding.busy || onboarding.building;
    $('ctx-ob-progress').hidden = !onboarding.building;
    $('ctx-ob-finish').setAttribute('aria-busy', String(onboarding.busy || onboarding.building));
  };
  const updateEmbedderFields = () => {
    const backend = $('ctx-ob-backend').value;
    $('ctx-ob-cloud-fields').hidden = backend !== 'cloud';
    $('ctx-ob-local-fields').hidden = backend === 'cloud';
    const saved = (onboarding.data || {}).embedder || {};
    const sameEnv = $('ctx-ob-cloud-key-env').value === (saved.cloud_api_key_env || '');
    obBadge('ctx-ob-key-status', !sameEnv ? 'Save setup to check this environment name'
      : (saved.cloud_api_key_present ? 'Key present in server environment' : 'Key not present in server environment'),
    sameEnv && saved.cloud_api_key_present ? 'ready' : 'attention');
    const sameProfile = $('ctx-ob-profile').value === saved.profile;
    obText('ctx-ob-model-status', sameProfile && saved.model_cached
      ? 'Selected model is cached on this machine.' : 'Model may need a download when dense indexing starts. Lexical search needs no model.');
  };
  const renderOnboardingState = (data) => {
    onboarding.data = data;
    onboarding.building = data.rebuilding === true;
    setOnboardingReadiness(data.survey_required);
    obText('ctx-ob-home', data.home_dir || 'Not reported');
    obText('ctx-ob-phrases-path', data.phrases_path || 'Not reported');
    obText('ctx-ob-config-path', data.config_path || 'Not reported');
    const sources = $('ctx-ob-source-list');
    sources.replaceChildren();
    (data.sources || []).forEach((source) => {
      const item = document.createElement('li');
      const name = document.createElement('strong'); name.textContent = source.label || source.agent;
      const status = document.createElement('span'); status.className = 'ob-status';
      status.textContent = source.detected ? String(source.session_files || 0) + ' session files on disk' : 'Not detected';
      status.dataset.tone = source.detected ? 'ready' : '';
      const path = document.createElement('code'); path.textContent = source.root_path || 'No directory reported';
      item.append(name, status, path); sources.appendChild(item);
    });
    if (!sources.children.length) {
      const item = document.createElement('li'); item.textContent = 'No agent source directories were reported on this machine.'; sources.appendChild(item);
    }
    const services = data.services || {};
    obText('ctx-ob-mcp-address', '127.0.0.1:' + (services.mcp_port || 8044));
    obBadge('ctx-ob-mcp-status', services.mcp_service_healthy ? 'Healthy' : (services.mcp_service_installed ? 'Installed · not healthy' : 'Not installed'), services.mcp_service_healthy ? 'ready' : 'attention');
    obBadge('ctx-ob-reindex-status', services.reindex_schedule_installed ? 'Scheduled' : 'Not installed', services.reindex_schedule_installed ? 'ready' : 'attention');
    const index = data.index || {};
    obText('ctx-ob-catalog-count', String(index.catalog_sessions || 0));
    obText('ctx-ob-index-count', String(index.indexed_documents || 0));
    // Polling refreshes facts, never edits-in-progress or client selections.
    if (!onboarding.hydrated) {
      const fill = (id, value) => { if (!onboarding.editedFields.has(id)) $(id).value = value; };
      const phrases = data.phrases || {};
      Object.keys(phraseFields).forEach((key) => { fill(phraseFields[key], (phrases[key] || []).join('\n')); });
      const embedder = data.embedder || {};
      fill('ctx-ob-backend', embedder.backend || 'auto');
      fill('ctx-ob-profile', embedder.profile || 'base');
      fill('ctx-ob-cloud-url', embedder.cloud_url || '');
      fill('ctx-ob-cloud-model', embedder.cloud_model || '');
      fill('ctx-ob-cloud-key-env', embedder.cloud_api_key_env || '');
      const clients = $('ctx-ob-mcp-clients'); clients.replaceChildren();
      (data.mcp_clients || []).forEach((client) => {
        const label = document.createElement('label'); label.className = 'ob-check';
        const checkbox = document.createElement('input'); checkbox.type = 'checkbox'; checkbox.value = client.id;
        checkbox.checked = !!client.detected; checkbox.disabled = !client.detected;
        const description = document.createElement('span');
        description.textContent = (client.label || client.id) + (client.detected ? (client.wired ? ' · wired' : ' · available to wire') : ' · not detected');
        const path = document.createElement('small'); path.className = 'meta'; path.textContent = client.config_path || '';
        description.appendChild(path); label.append(checkbox, description); clients.appendChild(label);
      });
      if (!clients.children.length) clients.textContent = 'No supported MCP clients detected.';
      onboarding.hydrated = true;
    }
    updateEmbedderFields();
    updateOnboardingButtons();
    obText('ctx-ob-status', data.rebuilding ? 'Building your corpus. You can keep reading setup while it runs.'
      : (data.ready ? 'This machine is configured. Review or update your setup below.' : 'Review the five pillars, then complete setup.'));
    if (data.rebuilding) {
      obText('ctx-ob-finish-status', 'Building catalog and index… ' + (index.catalog_sessions || 0) + ' sessions · ' + (index.indexed_documents || 0) + ' indexed documents.');
    } else if (onboarding.finishing) {
      onboarding.finishing = false;
      if (data.ready && !data.survey_required) {
        obText('ctx-ob-finish-status', 'Setup complete. Your corpus is ready.');
        if (state.view === 'setup') showSessions();
        else if (state.view === 'sessions') loadBrowseData();
      } else {
        obText('ctx-ob-finish-status', 'The build stopped, but setup is not ready. Review the service and index status, then retry.');
      }
    } else if (!onboarding.busy) {
      obText('ctx-ob-finish-status', data.ready ? 'Ready to update settings and rebuild your corpus.' : 'Ready to save settings and build your first index.');
    }
  };
  const loadOnboardingState = () => {
    if (onboarding.loading) return onboarding.loading;
    onboarding.loading = onboardingJson('/api/onboarding/state').then((data) => {
      if (typeof data.ready !== 'boolean' || typeof data.survey_required !== 'boolean') throw new Error('Unrecognized onboarding state response');
      renderOnboardingState(data);
      return data;
    }).finally(() => { onboarding.loading = null; });
    return onboarding.loading;
  };
  const scheduleOnboardingPoll = () => {
    clearTimeout(onboarding.pollTimer);
    if (!onboarding.building) return;
    onboarding.pollTimer = setTimeout(async () => {
      try { await loadOnboardingState(); }
      catch (error) { obText('ctx-ob-finish-status', 'Could not refresh build status: ' + error.message + '. Retrying…'); }
      scheduleOnboardingPoll();
    }, 1500);
  };
  const showPhraseSuggestions = (data) => {
    obText('ctx-ob-discovery-status', String(data.scanned_sessions || 0) + ' sessions · ' + String(data.scanned_utterances || 0) + ' utterances scanned on disk.');
    const samples = $('ctx-ob-matched-samples'); samples.replaceChildren();
    data.matched_samples.forEach((sample) => {
      const item = document.createElement('li'); item.textContent = sample.text;
      const meta = document.createElement('small'); meta.textContent = sample.entry_type + ' · confidence ' + sample.confidence;
      item.appendChild(meta); samples.appendChild(item);
    });
    if (!samples.children.length) {
      const item = document.createElement('li'); item.textContent = 'No matched utterances in the scanned sessions. Paste your own lines into the preview below.'; samples.appendChild(item);
    }
    const chips = $('ctx-ob-candidates'); chips.replaceChildren();
    data.candidate_openers.forEach((candidate) => {
      const bucket = Object.hasOwn(phraseFields, candidate.suggested_bucket) ? candidate.suggested_bucket : 'intent_keywords';
      const chip = document.createElement('button'); chip.type = 'button'; chip.className = 'ob-chip';
      chip.textContent = candidate.phrase + ' · ' + candidate.count + ' · ' + bucket.replace(/_/g, ' ');
      chip.title = candidate.sample_line || candidate.phrase;
      chip.addEventListener('click', () => {
        const input = $(phraseFields[bucket]);
        const lines = phraseLines(phraseFields[bucket]);
        if (!lines.includes(candidate.phrase)) input.value = lines.concat([candidate.phrase]).join('\n');
        onboarding.editedFields.add(input.id);
        chip.disabled = true; chip.textContent = 'Added · ' + candidate.phrase;
        obText('ctx-ob-phrase-status', 'Added to ' + bucket.replace(/_/g, ' ') + '. Save personal phrases to activate it.');
      });
      chips.appendChild(chip);
    });
    if (!chips.children.length) chips.textContent = 'No candidate openers found in this sample.';
  };
  const loadPhraseSuggestions = (force) => {
    if (onboarding.discovering) return onboarding.discovering;
    if (!force && onboarding.suggestions) return Promise.resolve();
    $('ctx-ob-discover-phrases').disabled = true;
    obText('ctx-ob-discovery-status', 'Scanning real session utterances on disk…');
    onboarding.discovering = onboardingJson('/api/onboarding/phrase-suggestions').then((data) => {
      if (!Array.isArray(data.matched_samples) || !Array.isArray(data.candidate_openers)) throw new Error('Unrecognized phrase discovery response');
      onboarding.suggestions = data; showPhraseSuggestions(data);
    }).catch((error) => {
      obText('ctx-ob-discovery-status', 'Discovery unavailable: ' + error.message + '. Use Discover to retry.');
    }).finally(() => { onboarding.discovering = null; $('ctx-ob-discover-phrases').disabled = false; });
    return onboarding.discovering;
  };
  const showSetup = () => {
    state.view = 'setup';
    markNav($('ctx-nav-setup'));
    showContinuity(false);
    $('ctx-layout').hidden = true;
    $('ctx-onboarding-studio').hidden = false;
    document.body.classList.add('setup-view');
    $('ctx-ob-title').focus();
    loadOnboardingState().then(scheduleOnboardingPoll).catch((error) => {
      obText('ctx-ob-status', 'Setup status unavailable: ' + error.message + '. Refresh setup status to retry.');
    });
    loadPhraseSuggestions(false);
  };
  const savePersonalPhrases = async () => {
    if (!onboarding.data || onboarding.busy || onboarding.building) return;
    onboarding.busy = true; updateOnboardingButtons();
    obText('ctx-ob-phrase-status', 'Saving personal phrases…');
    try {
      await onboardingJson('/api/onboarding', Object.assign(personalPhrases(), {
        install_services: false, install_reindex_schedule: false, wire_mcp_clients: false,
        selected_mcp_clients: [], build_initial_index: false,
      }));
      obText('ctx-ob-phrase-status', 'Personal phrases saved. Preview now uses your additions.');
      await loadOnboardingState();
    } catch (error) { obText('ctx-ob-phrase-status', 'Could not save phrases: ' + error.message); }
    finally { onboarding.busy = false; updateOnboardingButtons(); }
  };
  const completeOnboarding = async () => {
    if (!onboarding.data || onboarding.busy || onboarding.building) return;
    const clients = Array.from($('ctx-ob-mcp-clients').querySelectorAll('input:checked')).map((input) => input.value);
    const payload = Object.assign(personalPhrases(), {
      embedder: {
        backend: $('ctx-ob-backend').value, profile: $('ctx-ob-profile').value,
        cloud_url: $('ctx-ob-cloud-url').value.trim(), cloud_model: $('ctx-ob-cloud-model').value.trim(),
        cloud_api_key_env: $('ctx-ob-cloud-key-env').value.trim(),
      },
      install_services: $('ctx-ob-install-services').checked,
      install_reindex_schedule: $('ctx-ob-install-reindex').checked,
      wire_mcp_clients: clients.length > 0, selected_mcp_clients: clients, build_initial_index: true,
    });
    if (payload.embedder.backend === 'cloud' && (!payload.embedder.cloud_url || !payload.embedder.cloud_model)) {
      obText('ctx-ob-finish-status', 'Enter the HTTP endpoint URL and model before completing setup.'); return;
    }
    onboarding.busy = true; updateOnboardingButtons();
    obText('ctx-ob-finish-status', 'Saving settings and starting the initial index build…');
    try {
      // Drain any earlier status read so it cannot settle this new build.
      if (onboarding.loading) await onboarding.loading;
      const result = await onboardingJson('/api/onboarding', payload);
      if (!result.index_started && !result.rebuilding) throw new Error('Settings saved, but the index build was not started');
      onboarding.finishing = true; onboarding.building = true; updateOnboardingButtons();
      obText('ctx-ob-finish-status', 'Index build started. Waiting for catalog and index status…');
      try { await loadOnboardingState(); }
      catch (error) { obText('ctx-ob-finish-status', 'Build accepted; status unavailable: ' + error.message + '. Retrying…'); }
      scheduleOnboardingPoll();
    } catch (error) { obText('ctx-ob-finish-status', 'Setup could not complete: ' + error.message); }
    finally { onboarding.busy = false; updateOnboardingButtons(); }
  };
  const previewOnboardingPhrases = async () => {
    const lines = phraseLines('ctx-ob-preview-input');
    const results = $('ctx-ob-preview-results'); results.replaceChildren();
    const message = (text) => { const item = document.createElement('li'); item.textContent = text; results.appendChild(item); };
    if (!lines.length) { message('Paste at least one sample line to test.'); return; }
    $('ctx-ob-preview-btn').disabled = true; message('Testing against the active dictionary…');
    try {
      const data = await onboardingJson('/api/onboarding/preview-phrases', { lines: lines });
      if (!Array.isArray(data.results)) throw new Error('Unrecognized preview response');
      results.replaceChildren();
      data.results.forEach((result) => {
        const item = document.createElement('li'); item.textContent = result.line;
        const meta = document.createElement('small');
        meta.textContent = result.matched ? (result.entry_types || []).join(', ') + ' · confidence ' + result.max_confidence : 'No match';
        item.appendChild(meta); results.appendChild(item);
      });
    } catch (error) { results.replaceChildren(); message('Preview unavailable: ' + error.message); }
    finally { $('ctx-ob-preview-btn').disabled = false; }
  };

  const studio = () => {
    $('ctx-onboarding-studio').querySelectorAll('textarea, input, select').forEach((input) => {
      ['input', 'change'].forEach((event) => input.addEventListener(event, () => onboarding.editedFields.add(input.id)));
    });
    const openStudio = $('ctx-open-onboarding-studio');
    if (openStudio) openStudio.addEventListener('click', showSetup);
    $('ctx-ob-discover-phrases').addEventListener('click', () => loadPhraseSuggestions(true));
    $('ctx-ob-save-phrases').addEventListener('click', savePersonalPhrases);
    $('ctx-ob-preview-btn').addEventListener('click', previewOnboardingPhrases);
    $('ctx-ob-complete-btn').addEventListener('click', completeOnboarding);
    $('ctx-ob-backend').addEventListener('change', updateEmbedderFields);
    $('ctx-ob-profile').addEventListener('change', updateEmbedderFields);
    $('ctx-ob-cloud-key-env').addEventListener('input', updateEmbedderFields);
    $('ctx-ob-retry').addEventListener('click', () => {
      loadOnboardingState().then(scheduleOnboardingPoll).catch((error) => {
        obText('ctx-ob-status', 'Setup status unavailable: ' + error.message);
      });
    });
    const onboard = $('ctx-onboarding');
    const survey = $('ctx-onboarding-phrases');
    const surveySave = $('ctx-onboarding-save');
    const surveyStatus = $('ctx-onboarding-status');
    if (surveySave && survey) surveySave.addEventListener('click', async () => {
      if (onboarding.busy || onboarding.building) return;
      const phrases = survey.value.split(/\n/).map((line) => line.trim()).filter(Boolean);
      onboarding.busy = true; updateOnboardingButtons();
      try {
        const body = await onboardingJson('/api/onboarding', { phrases: phrases });
        const service = body.service ? ' Background service: ' + body.service + '.' : '';
        if (surveyStatus) surveyStatus.textContent = 'Saved on this machine.' + service;
        if (onboard) onboard.open = false;
        loadOnboardingState().then(scheduleOnboardingPoll).catch(() => {});
      } catch (error) { if (surveyStatus) surveyStatus.textContent = 'Not saved: ' + error.message; }
      finally { onboarding.busy = false; updateOnboardingButtons(); }
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

  /* --- continuity: the same frozen window in the browser and CLI --------- */
  const continuityPanel = document.createElement('section');
  continuityPanel.id = 'ctx-continuity';
  continuityPanel.className = 'continuity-panel';
  continuityPanel.hidden = true;
  continuityPanel.setAttribute('aria-label', 'Continuity');
  continuityPanel.innerHTML = `
    <h2>Continuity</h2>
    <p class="meta">Build a cold start from this machine's session history. Read the sections, then use the same pack in your terminal.</p>
    <form id="ctx-continuity-form" class="continuity-form">
      <label>Project<input name="project" id="ctx-continuity-project" placeholder="owner/repo" required list="ctx-continuity-projects" /></label>
      <datalist id="ctx-continuity-projects"></datalist>
      <label>Name (optional)<input name="as" placeholder="Andrzej" /></label>
      <label>Hours<input name="hours" type="number" min="0" step="1" value="72" required /></label>
      <label class="continuity-fit"><input name="inject" type="checkbox" checked />Fit to prompt</label>
      <button type="submit">Build pack</button>
    </form>
    <p id="ctx-continuity-status" class="meta" role="status" aria-live="polite">Choose a project to begin. Hours: 0 includes all history.</p>
    <div id="ctx-continuity-result" hidden>
      <p class="meta">Reproduce this window in the terminal (same AICX home and source history):</p>
      <pre class="continuity-command"><code id="ctx-continuity-command"></code></pre>
      <button id="ctx-continuity-copy" type="button">Copy command</button>
      <nav id="ctx-continuity-meter" class="continuity-meter" aria-label="Pack sections"></nav>
      <div id="ctx-continuity-sections"></div>
    </div>`;
  const mainBody = document.querySelector('.main-body');
  if (mainBody) mainBody.appendChild(continuityPanel);
  const navContinuity = document.createElement('button');
  navContinuity.type = 'button';
  navContinuity.id = 'ctx-nav-continuity';
  navContinuity.className = 'rail-nav-item';
  navContinuity.textContent = 'Continuity';
  const railNav = document.querySelector('.rail-nav');
  if (railNav) railNav.appendChild(navContinuity);
  const showContinuity = (visible) => {
    $('ctx-onboarding-studio').hidden = true;
    document.body.classList.remove('setup-view');
    if (visible) state.view = 'continuity';
    continuityPanel.hidden = !visible;
    const layout = $('ctx-layout');
    if (layout) layout.hidden = visible;
    if (visible) {
      const project = $('ctx-continuity-project');
      if (!project.value) project.value = state.project || '';
      const options = $('ctx-continuity-projects');
      options.replaceChildren();
      Array.from(ui.project.options).filter((o) => o.value).forEach((o) => {
        const option = document.createElement('option'); option.value = o.value; options.appendChild(option);
      });
      project.focus();
    }
  };
  navContinuity.addEventListener('click', () => { markNav(navContinuity); showContinuity(true); });
  $('ctx-continuity-form').addEventListener('submit', async (event) => {
    event.preventDefault();
    const form = event.currentTarget;
    const params = new URLSearchParams(new FormData(form));
    params.set('inject', form.elements.inject.checked ? 'true' : 'false');
    if (!params.get('as').trim()) params.delete('as');
    const button = form.querySelector('button[type="submit"]');
    const status = $('ctx-continuity-status');
    const result = $('ctx-continuity-result');
    button.disabled = true; result.hidden = true;
    status.textContent = 'Building continuity from live sources…';
    try {
      const response = await apiFetch('/api/continuity?' + params.toString());
      const data = await response.json();
      if (!response.ok || !data.ok) throw new Error(data.error || 'HTTP ' + response.status);
      $('ctx-continuity-command').textContent = data.cli_command;
      const meter = $('ctx-continuity-meter');
      const sections = $('ctx-continuity-sections');
      meter.replaceChildren(); sections.replaceChildren();
      data.sections.forEach((section, index) => {
        const id = 'ctx-continuity-section-' + index;
        const link = document.createElement('a');
        link.href = '#' + id;
        link.textContent = section.title + ' · ' + section.lines + ' lines';
        meter.appendChild(link);
        const article = document.createElement('article');
        article.id = id; article.className = 'continuity-section';
        const heading = document.createElement('h3');
        heading.textContent = section.title + ' · ' + section.lines + ' lines';
        const content = document.createElement('div'); content.className = 'md-rendered';
        content.innerHTML = renderMarkdown(section.markdown);
        article.append(heading, content); sections.appendChild(article);
      });
      status.textContent = data.total_lines + ' lines · window ends ' + data.until + (data.inject ? ' · fit to prompt' : ' · full pack');
      result.hidden = false;
    } catch (error) { status.textContent = error.message; }
    finally { button.disabled = false; }
  });
  $('ctx-continuity-copy').addEventListener('click', async () => {
    try { await navigator.clipboard.writeText($('ctx-continuity-command').textContent); }
    catch (_) { $('ctx-continuity-status').textContent = 'Select the command above to copy it.'; }
  });

  const loadIntents = () => {
    state.view = 'intents';
    state.mode = 'intents';
    ui.summary.textContent = 'Loading intents\u2026';
    const params = new URLSearchParams();
    if (state.project) params.set('project', state.project);
    if (state.agent) params.set('agent', state.agent);
    params.set('hours', String(state.intentHours));
    params.set('limit', '20');
    apiFetch('/api/intents?' + params.toString())
      .then(function(r) { return r.json(); })
      .then(function(data) {
        if (!data.ok) { ui.summary.textContent = 'Failed: ' + (data.error || 'unknown'); return; }
        const records = data.items || [];
        state.browseRecords = records;
        const rows = records.map(function(record) { return { record: record, score: 0 }; });
        state.rows = rows;
        ui.summary.textContent = Math.min(records.length, state.limit) + ' / ' + records.length + ' intents';
        if (ui.statFiles) ui.statFiles.textContent = String(records.length);
        renderList(rows);
      })
      .catch(function(err) {
        if (err && err.status === 401) return;
        ui.summary.textContent = 'Intents failed: ' + err.message;
      });
  };

  const navSessions = $('ctx-nav-sessions');
  const navIntents = $('ctx-nav-intents');
  const navSetup = $('ctx-nav-setup');
  const markNav = (active) => {
    document.querySelectorAll('.rail-nav-item').forEach(function(btn) {
      btn.classList.toggle('active', btn === active);
      if (btn === active) btn.setAttribute('aria-current', 'page');
      else btn.removeAttribute('aria-current');
    });
  };
  const showSessions = () => {
    markNav(navSessions);
    state.view = 'sessions';
    showContinuity(false);
    setTimeBtnActive(state.since);
    if (ui.list) ui.list.scrollTop = 0;
    if (ui.search) ui.search.focus();
    loadBrowseData();
  };
  if (navSessions) navSessions.addEventListener('click', showSessions);
  if (navIntents) navIntents.addEventListener('click', function() {
    markNav(navIntents);
    showContinuity(false);
    setTimeBtnActive(state.intentHours === 0 ? '' : (state.intentHours === 720 ? '30d' : ''));
    loadIntents();
  });
  if (navSetup) navSetup.addEventListener('click', showSetup);

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
        applyStatus(data);
        const host = location.hostname;
        if (host === '127.0.0.1' || host === 'localhost' || host === '::1') {
          const access = document.getElementById('ctx-access');
          if (access) access.textContent = 'This machine. No sign-in.';
        }
        const onboard = $('ctx-onboarding');
        if (onboard && data.survey_required) onboard.open = true;
        if (data.rebuilding) showCorpusBusy();
        if (state.view !== 'setup') loadBrowseData();
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
