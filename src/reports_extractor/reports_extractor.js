
(function() {
  const readEmbedded = () => {
    const node = document.getElementById('rx-data');
    return node ? JSON.parse(node.textContent || '{}') : { records: [] };
  };

  const normalizeText = (value) => {
    const chars = {
      '\u0141': 'L', '\u0142': 'l', '\u0104': 'A', '\u0105': 'a',
      '\u0106': 'C', '\u0107': 'c', '\u0118': 'E', '\u0119': 'e',
      '\u0143': 'N', '\u0144': 'n', '\u00d3': 'O', '\u00f3': 'o',
      '\u015a': 'S', '\u015b': 's', '\u0179': 'Z', '\u017a': 'z',
      '\u017b': 'Z', '\u017c': 'z'
    };
    return String(value || '')
      .replace(/[\u0141\u0142\u0104\u0105\u0106\u0107\u0118\u0119\u0143\u0144\u00d3\u00f3\u015a\u015b\u0179\u017a\u017b\u017c]/g, (ch) => chars[ch] || ch)
      .toLowerCase();
  };

  const embedded = readEmbedded();
  const state = {
    base: embedded,
    payload: embedded,
    records: Array.isArray(embedded.records) ? embedded.records.slice() : [],
    selectedKey: null,
    query: '',
    filters: {
      workflow: '',
      lane: '',
      agent: '',
      status: '',
      day: ''
    }
  };

  const ui = {
    search: document.getElementById('rx-search'),
    workflow: document.getElementById('rx-workflow'),
    lane: document.getElementById('rx-lane'),
    agent: document.getElementById('rx-agent'),
    status: document.getElementById('rx-status'),
    day: document.getElementById('rx-day'),
    cards: document.getElementById('rx-cards'),
    summary: document.getElementById('rx-summary'),
    list: document.getElementById('rx-list'),
    detailTitle: document.getElementById('rx-detail-title'),
    detailMeta: document.getElementById('rx-detail-meta'),
    detailGrid: document.getElementById('rx-detail-grid'),
    detailHeadings: document.getElementById('rx-detail-headings'),
    detailPreview: document.getElementById('rx-detail-preview'),
    detailContent: document.getElementById('rx-detail-content'),
    assumptions: document.getElementById('rx-assumptions'),
    importTrigger: document.getElementById('rx-import-trigger'),
    importFile: document.getElementById('rx-import-file'),
    downloadBundle: document.getElementById('rx-download-bundle'),
    resetData: document.getElementById('rx-reset-data'),
    copyPath: document.getElementById('rx-copy-path')
  };

  const selectOptions = (node, values, placeholder) => {
    const current = node.value;
    node.innerHTML = '';
    const opt = document.createElement('option');
    opt.value = '';
    opt.textContent = placeholder;
    node.appendChild(opt);
    values.forEach((value) => {
      const entry = document.createElement('option');
      entry.value = value;
      entry.textContent = value;
      node.appendChild(entry);
    });
    node.value = values.includes(current) ? current : '';
  };

  const updateFilterOptions = () => {
    const payload = state.payload || { workflows: [], lanes: [], agents: [], statuses: [], days: [] };
    selectOptions(ui.workflow, payload.workflows || [], 'All workflows');
    selectOptions(ui.lane, payload.lanes || [], 'All lanes');
    selectOptions(ui.agent, payload.agents || [], 'All agents');
    selectOptions(ui.status, payload.statuses || [], 'All statuses');
    selectOptions(ui.day, payload.days || [], 'All days');
  };

  const MERGE_MAX_RECORDS = 100000;
  const MERGE_MAX_FIELD_CHARS = 16384;
  const MERGE_REQUIRED_FIELDS = ['key', 'workflow', 'status', 'agent', 'date_iso', 'title', 'relative_path'];

  const validateImportedRecord = (record, index) => {
    if (!record || typeof record !== 'object' || Array.isArray(record)) {
      throw new Error('Imported record ' + index + ' is not an object.');
    }
    for (let i = 0; i < MERGE_REQUIRED_FIELDS.length; i += 1) {
      const field = MERGE_REQUIRED_FIELDS[i];
      const value = record[field];
      if (typeof value !== 'string') {
        throw new Error('Imported record ' + index + ' is missing required string field "' + field + '".');
      }
      if (value.length > MERGE_MAX_FIELD_CHARS) {
        throw new Error('Imported record ' + index + ' field "' + field + '" exceeds ' + MERGE_MAX_FIELD_CHARS + ' chars.');
      }
    }
    return record;
  };

  const mergePayload = (incoming) => {
    if (!incoming || typeof incoming !== 'object' || !Array.isArray(incoming.records)) {
      throw new Error('Imported file does not look like an AICX reports bundle.');
    }
    if (incoming.schema_version !== undefined && incoming.schema_version !== 1) {
      throw new Error('Imported bundle schema_version ' + incoming.schema_version + ' is not supported (expected 1).');
    }
    if (incoming.records.length > MERGE_MAX_RECORDS) {
      throw new Error('Imported bundle has ' + incoming.records.length + ' records, exceeding limit of ' + MERGE_MAX_RECORDS + '.');
    }
    incoming.records.forEach(validateImportedRecord);
    const merged = new Map();
    [...(state.payload.records || []), ...incoming.records].forEach((record) => {
      merged.set(record.key || record.absolute_path || record.relative_path, record);
    });
    const records = Array.from(merged.values()).sort((a, b) => (b.sort_ts || 0) - (a.sort_ts || 0));
    state.payload = {
      schema_version: incoming.schema_version || state.payload.schema_version || 1,
      generated_at: incoming.generated_at || state.payload.generated_at,
      artifacts_root: incoming.artifacts_root || state.payload.artifacts_root,
      resolved_org: incoming.resolved_org || state.payload.resolved_org,
      resolved_repo: incoming.resolved_repo || state.payload.resolved_repo,
      scan_root: incoming.scan_root || state.payload.scan_root,
      selected_date: state.payload.selected_date,
      selected_workflow: state.payload.selected_workflow,
      stats: state.payload.stats || {},
      assumptions: Array.from(new Set([...(state.payload.assumptions || []), ...(incoming.assumptions || [])])),
      workflows: Array.from(new Set(records.map((record) => record.workflow).filter(Boolean))).sort(),
      agents: Array.from(new Set(records.map((record) => record.agent).filter(Boolean))).sort(),
      statuses: Array.from(new Set(records.map((record) => record.status).filter(Boolean))).sort(),
      lanes: Array.from(new Set(records.map((record) => record.lane).filter(Boolean))).sort(),
      days: Array.from(new Set(records.map((record) => record.date_iso).filter(Boolean))).sort(),
      records
    };
    state.records = records;
    updateFilterOptions();
    render();
  };

  const filteredRecords = () => {
    const query = normalizeText(state.query);
    return (state.payload.records || []).filter((record) => {
      if (state.filters.workflow && record.workflow !== state.filters.workflow) return false;
      if (state.filters.lane && record.lane !== state.filters.lane) return false;
      if (state.filters.agent && record.agent !== state.filters.agent) return false;
      if (state.filters.status && record.status !== state.filters.status) return false;
      if (state.filters.day && record.date_iso !== state.filters.day) return false;
      if (!query) return true;
      return normalizeText(record.search_blob || '').includes(query);
    });
  };

  const metricCard = (label, value) => {
    const div = document.createElement('div');
    div.className = 'metric';
    const strong = document.createElement('strong');
    strong.textContent = String(value);
    const span = document.createElement('span');
    span.textContent = label;
    div.appendChild(strong);
    div.appendChild(span);
    return div;
  };

  const statusClass = (status) => {
    const normalized = String(status || '').toLowerCase();
    if (normalized === 'completed') return 'ok';
    if (normalized === 'launching' || normalized === 'planned' || normalized === 'running') return 'warn';
    return normalized ? 'danger' : '';
  };

  const renderCards = (records) => {
    ui.cards.innerHTML = '';
    const complete = records.filter((record) => String(record.status).toLowerCase() === 'completed').length;
    const partial = records.length - complete;
    const metaOnly = records.filter((record) => record.has_meta && !record.has_markdown).length;
    const workflows = new Set(records.map((record) => record.workflow).filter(Boolean)).size;
    const agents = new Set(records.map((record) => record.agent).filter(Boolean)).size;
    [
      ['visible records', records.length],
      ['completed', complete],
      ['partial/incomplete', partial],
      ['meta only', metaOnly],
      ['workflows', workflows || 0],
      ['agents', agents || 0]
    ].forEach(([label, value]) => ui.cards.appendChild(metricCard(label, value)));
  };

  const renderList = (records) => {
    ui.list.innerHTML = '';
    if (!records.length) {
      const empty = document.createElement('div');
      empty.className = 'empty-state';
      empty.textContent = 'No artifacts matched the current filters.';
      ui.list.appendChild(empty);
      return;
    }

    records.forEach((record) => {
      const button = document.createElement('button');
      button.type = 'button';
      button.className = 'result-item' + (record.key === state.selectedKey ? ' active' : '');
      button.addEventListener('click', () => {
        state.selectedKey = record.key;
        render();
      });

      const title = document.createElement('div');
      title.className = 'result-title';
      title.textContent = record.title || record.file_name || 'artifact';
      button.appendChild(title);

      const badges = document.createElement('div');
      badges.className = 'badge-row';
      [
        record.workflow,
        record.lane,
        record.status,
        record.agent,
        record.date_iso
      ].filter(Boolean).forEach((value, idx) => {
        const badge = document.createElement('span');
        badge.className = 'badge' + (idx === 2 ? ' ' + statusClass(value) : '');
        badge.textContent = value;
        badges.appendChild(badge);
      });
      button.appendChild(badges);

      const preview = document.createElement('p');
      preview.className = 'result-preview';
      preview.textContent = record.preview || '';
      button.appendChild(preview);

      ui.list.appendChild(button);
    });
  };

  const detailCell = (label, value) => {
    if (!value) return null;
    const div = document.createElement('div');
    div.className = 'detail-cell';
    const strong = document.createElement('strong');
    strong.textContent = label;
    const span = document.createElement('span');
    span.textContent = String(value);
    div.appendChild(strong);
    div.appendChild(span);
    return div;
  };

  const renderDetail = (record) => {
    if (!record) {
      ui.detailTitle.textContent = 'Select a record';
      ui.detailMeta.textContent = '';
      ui.detailGrid.innerHTML = '';
      ui.detailHeadings.innerHTML = '';
      ui.detailPreview.textContent = '';
      ui.detailContent.textContent = 'Use search or filters to inspect a workflow artifact.';
      return;
    }

    ui.detailTitle.textContent = record.title || record.file_name || 'artifact';
    ui.detailMeta.textContent = [record.workflow, record.lane, record.status, record.agent, record.date_iso].filter(Boolean).join(' • ');
    ui.detailPreview.textContent = record.preview || '';
    ui.detailContent.textContent = record.detail_text || '';
    ui.copyPath.dataset.path = record.absolute_path || '';

    ui.detailGrid.innerHTML = '';
    [
      ['absolute path', record.absolute_path],
      ['relative path', record.relative_path],
      ['run id', record.run_id],
      ['prompt id', record.prompt_id],
      ['skill code', record.skill_code],
      ['mode', record.mode],
      ['completed at', record.completed_at],
      ['updated at', record.updated_at],
      ['duration (s)', record.duration_s],
      ['session id', record.session_id],
      ['transcript', record.transcript_path],
      ['launcher', record.launcher_path]
    ].forEach(([label, value]) => {
      const cell = detailCell(label, value);
      if (cell) ui.detailGrid.appendChild(cell);
    });

    ui.detailHeadings.innerHTML = '';
    (record.headings || []).forEach((heading) => {
      const chip = document.createElement('span');
      chip.className = 'chip';
      chip.textContent = heading;
      ui.detailHeadings.appendChild(chip);
    });
  };

  const renderAssumptions = () => {
    ui.assumptions.innerHTML = '';
    (state.payload.assumptions || []).forEach((item) => {
      const li = document.createElement('li');
      li.textContent = item;
      ui.assumptions.appendChild(li);
    });
  };

  const render = () => {
    const records = filteredRecords();
    renderCards(records);
    renderList(records);
    const selected = records.find((record) => record.key === state.selectedKey) || records[0] || null;
    if (selected) {
      state.selectedKey = selected.key;
    }
    renderDetail(selected);
    renderAssumptions();
    ui.summary.textContent = `Showing ${records.length} of ${(state.payload.records || []).length} records from ${state.payload.resolved_org || ''}/${state.payload.resolved_repo || ''}.`;
  };

  ui.search.addEventListener('input', () => {
    state.query = ui.search.value || '';
    render();
  });
  [['workflow', ui.workflow], ['lane', ui.lane], ['agent', ui.agent], ['status', ui.status], ['day', ui.day]].forEach(([key, node]) => {
    node.addEventListener('change', () => {
      state.filters[key] = node.value || '';
      render();
    });
  });

  ui.importTrigger.addEventListener('click', () => ui.importFile.click());
  ui.importFile.addEventListener('change', async () => {
    const file = ui.importFile.files && ui.importFile.files[0];
    if (!file) return;
    const text = await file.text();
    mergePayload(JSON.parse(text));
    ui.importFile.value = '';
  });

  ui.downloadBundle.addEventListener('click', () => {
    const blob = new Blob([JSON.stringify(state.payload, null, 2)], { type: 'application/json' });
    const url = URL.createObjectURL(blob);
    const link = document.createElement('a');
    link.href = url;
    link.download = `${(state.payload.resolved_repo || 'aicx-reports').replace(/[^a-z0-9_-]+/gi, '-')}.bundle.json`;
    document.body.appendChild(link);
    link.click();
    link.remove();
    URL.revokeObjectURL(url);
  });

  ui.resetData.addEventListener('click', () => {
    state.payload = state.base;
    state.records = Array.isArray(state.base.records) ? state.base.records.slice() : [];
    state.selectedKey = null;
    updateFilterOptions();
    render();
  });

  ui.copyPath.addEventListener('click', async () => {
    const path = ui.copyPath.dataset.path || '';
    if (!path) return;
    try {
      await navigator.clipboard.writeText(path);
      ui.copyPath.textContent = 'Copied';
      setTimeout(() => { ui.copyPath.textContent = 'Copy Path'; }, 900);
    } catch (_) {
      ui.copyPath.textContent = path;
    }
  });

  updateFilterOptions();
  render();
})();
