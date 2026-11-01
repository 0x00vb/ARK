import { h, clear } from './dom.js';
import { readCalendar, calendarRange, calendarOnDay, dateInZone, shiftDate } from './calendar-data.js';

const DELIVERY = { deadline: 'Deadline', manual_reminder: 'Manual reminder', agent_run: 'Scheduled work', provider_schedule: 'Service schedule' };
const STATES = { draft: 'Draft', planned: 'Planned', paused: 'Paused', running: 'Running', needs_you: 'Needs you', completed: 'Completed', failed: 'Failed', cancelled: 'Cancelled', unknown: 'Unknown', published: 'Reported published' };
const dateLabel = date => new Intl.DateTimeFormat(undefined, { timeZone: 'UTC', weekday: 'short', year: 'numeric', month: 'short', day: 'numeric' }).format(new Date(`${date}T00:00:00Z`));

export function registerCalendar(ark) {
  ark.artifactViewer({
    id: 'calendar', label: 'Calendar', primary: true, wide: true, presents: ['calendar'],
    match: artifact => /\.calendar\.json$/i.test(artifact.name),
    render(el, artifact, api) {
      let alive = true, control;
      const status = h('p', { role: 'status' }, 'Loading calendar'); el.append(status);
      const load = async () => {
        if (!Number.isSafeInteger(artifact.size) || artifact.size < 0 || artifact.size > 1048576) throw new Error('Calendar exceeds the 1 MiB limit');
        const result = await api.call('artifact', { id: artifact.id, offset: 0, length: 1048576, hex: true });
        if (!alive) return;
        if (result.artifact?.id !== artifact.id || result.artifact?.kind !== artifact.kind || result.artifact?.name !== artifact.name || result.artifact?.size !== artifact.size || result.total !== artifact.size || result.offset !== 0 || result.length !== artifact.size || typeof result.hex !== 'string' || result.hex.length !== artifact.size * 2 || !/^(?:[0-9a-f]{2})*$/i.test(result.hex)) throw new Error('Calendar snapshot is incomplete or changed');
        const bytes = Uint8Array.from(result.hex.match(/../g) || [], byte => parseInt(byte, 16));
        let text;
        try { text = new TextDecoder('utf-8', { fatal: true }).decode(bytes); }
        catch { throw new Error('Calendar is not valid UTF-8'); }
        status.remove(); control = calendarView(el, { text }, 'Raw');
      };
      void load().catch(error => { if (alive) status.textContent = String(error.message); });
      return { focus: () => control?.focus(), dispose() { alive = false; control?.dispose(); status.remove(); } };
    },
  });
}

export function calendarView(el, file, sourceLabel = 'Source', options = {}) {
  const root = h('section.cal', { 'aria-label': 'Calendar' });
  el.append(root);
  let data, date, mode = 'month', selected = null;
  const eventButtons = new Map();
  let lastRange = '';
  const changedRange = () => {
    const days = calendarRange(date, mode), range = `${days[0]}:${days.at(-1)}`;
    if (range !== lastRange) { lastRange = range; options.range?.(days[0], shiftDate(days.at(-1), 1)); }
  };
  const time = ms => new Intl.DateTimeFormat(undefined, { timeZone: data.timezone, hour: 'numeric', minute: '2-digit', timeZoneName: 'short' }).format(ms);

  function move(direction) {
    selected = null;
    if (mode === 'month') {
      const next = new Date(`${date.slice(0, 7)}-01T00:00:00Z`);
      next.setUTCMonth(next.getUTCMonth() + direction);
      date = next.toISOString().slice(0, 10);
    } else date = shiftDate(date, direction * (mode === 'week' ? 7 : 1));
    draw();
    root.querySelector(`[aria-label="${direction < 0 ? 'Previous' : 'Next'} period"]`).focus();
  }

  function details(event, body) {
    const back = h('button.btn.ghost.small', { type: 'button', onclick: () => { selected = null; draw(); eventButtons.get(event.id)?.focus(); } }, 'Back to calendar');
    const heading = h('h2', { tabindex: '-1' }, event.title);
    const rows = h('dl.cal-facts');
    const when = event.end_known === false ? (event.all_day ? event.start : `${dateLabel(dateInZone(event.start_ms, data.timezone))} ${time(event.start_ms)}`) : event.all_day ? `${event.start} to ${event.end} (end excluded)` : `${dateLabel(dateInZone(event.start_ms, data.timezone))} ${time(event.start_ms)} to ${dateLabel(dateInZone(event.end_ms, data.timezone))} ${time(event.end_ms)}`;
    for (const [key, value] of [['When', when], ['Timezone', data.timezone], ['Delivery', DELIVERY[event.delivery]], ['State', STATES[event.state]], ['Account', event.account], ['Next action', event.next_action], ['Draft', event.draft], ['Receipt', event.receipt], ['Source', event.source_record], ['Revision', event.source_revision]]) {
      if (value) rows.append(h('dt', key), h('dd', value));
    }
    const open = options.open ? h('button.btn', { type: 'button', onclick: () => options.open(event) }, event.id.startsWith('run-') || event.id.startsWith('automation-ar-') ? 'Open work' : 'Open automation') : null;
    body.append(h('section.cal-detail', { 'aria-label': 'Item details' }, back, heading, rows, open));
    heading.focus();
  }

  function draw() {
    clear(root); eventButtons.clear();
    if (!data) return;
    changedRange();
    const label = mode === 'month'
      ? new Intl.DateTimeFormat(undefined, { timeZone: 'UTC', month: 'long', year: 'numeric' }).format(new Date(`${date}T00:00:00Z`))
      : mode === 'week' ? `${dateLabel(calendarRange(date, mode)[0])} to ${dateLabel(calendarRange(date, mode)[6])}` : dateLabel(date);
    const modes = h('div.cal-modes', { role: 'group', 'aria-label': 'Calendar view' });
    for (const value of ['month', 'week', 'day']) modes.append(h('button.btn.ghost.small', { type: 'button', 'aria-pressed': String(value === mode), onclick: () => { mode = value; selected = null; draw(); root.querySelector(`[data-mode="${value}"]`).focus(); }, 'data-mode': value }, value[0].toUpperCase() + value.slice(1)));
    const snapshot = new Intl.DateTimeFormat(undefined, { timeZone: data.timezone, dateStyle: 'medium', timeStyle: 'short' }).format(data.generated_ms);
    root.append(h('header.cal-head', h('div', h('h1', data.title), h('span.cal-snapshot', `Snapshot ${snapshot} · ${data.timezone}`)), modes));
    const navigation = h('nav.cal-nav', { 'aria-label': 'Calendar dates' },
      h('button.btn.ghost.small', { type: 'button', 'aria-label': 'Previous period', onclick: () => move(-1) }, 'Previous'),
      h('button.btn.ghost.small', { type: 'button', onclick: () => { date = dateInZone(Date.now(), data.timezone); selected = null; draw(); root.querySelector('.cal-nav button:nth-child(2)').focus(); } }, 'Today'),
      h('button.btn.ghost.small', { type: 'button', 'aria-label': 'Next period', onclick: () => move(1) }, 'Next'),
      h('h2', { 'aria-live': 'polite', tabindex: '-1' }, label));
    root.append(navigation);
    const body = h('div.cal-body'); root.append(body);
    if (selected) { details(selected, body); return; }
    const days = calendarRange(date, mode);
    if (mode === 'month') body.append(h('div.cal-weekdays', { 'aria-hidden': 'true' }, Array.from({ length: 7 }, (_, i) => h('span', new Intl.DateTimeFormat(undefined, { timeZone: 'UTC', weekday: 'short' }).format(new Date(`2026-01-${String(5 + i).padStart(2, '0')}T00:00:00Z`))))));
    const grid = h(`div.cal-days.${mode}`, { 'aria-label': label });
    const today = dateInZone(Date.now(), data.timezone);
    for (const day of days) {
      const items = data.events.filter(event => calendarOnDay(event, day, data.timezone));
      const openDay = () => { date = day; mode = 'day'; draw(); root.querySelector('.cal-nav h2').focus(); };
      const dayButton = h('button.cal-date', { type: 'button', 'aria-label': dateLabel(day), 'aria-current': day === today ? 'date' : null, onclick: openDay }, mode === 'month' ? [h('span.cal-date-number', String(Number(day.slice(-2)))), h('span.cal-date-label', dateLabel(day))] : dateLabel(day));
      const cell = h('section.cal-date-group', { 'data-outside': String(day.slice(0, 7) !== date.slice(0, 7)), 'data-empty': String(!items.length), 'aria-label': dateLabel(day) }, dayButton);
      const visible = mode === 'month' ? items.slice(0, 3) : items;
      for (const event of visible) {
        const button = h('button.cal-event', { type: 'button', 'data-state': event.state, onclick: () => { selected = event; draw(); } },
          h('span.cal-event-title', event.title), h('span.cal-event-time', `${event.all_day ? 'All day' : time(event.start_ms)} · ${DELIVERY[event.delivery]}`));
        eventButtons.set(event.id, button); cell.append(button);
      }
      if (visible.length < items.length) cell.append(h('button.cal-date', { type: 'button', 'aria-label': `${items.length - visible.length} more items on ${dateLabel(day)}`, onclick: openDay }, `${items.length - visible.length} more`));
      grid.append(cell);
    }
    if (!data.events.some(event => days.some(day => calendarOnDay(event, day, data.timezone)))) body.append(h('p.cal-empty', 'No items in this period'));
    body.append(grid);
  }

  function update(next) {
    try {
      data = readCalendar(next.text);
      date ||= dateInZone(Date.now(), data.timezone);
      if (selected) selected = data.events.find(event => event.id === selected.id) || null;
      draw();
    } catch (error) {
      data = null; selected = null; clear(root);
      root.append(h('h2', 'Calendar is unavailable'), h('p', String(error.message)), sourceLabel ? h('p', `Open ${sourceLabel} to inspect this file.`) : null);
    }
  }
  update(file);
  return { update, focus: () => ({ surface: { kind_tag: 'calendar', title: data?.title || 'Calendar' } }), dispose() { root.remove(); } };
}
