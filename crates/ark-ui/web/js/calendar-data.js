const STATES = new Set(['draft', 'planned', 'paused', 'running', 'needs_you', 'completed', 'failed', 'cancelled', 'unknown', 'published']);
const DELIVERY = new Set(['deadline', 'manual_reminder', 'agent_run', 'provider_schedule']);
const DAY = 86400000;

function text(value, name, max = 240) {
  if (typeof value !== 'string' || !value.trim() || value.length > max || /[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f]/.test(value)) throw new Error(`Invalid calendar ${name}`);
  return value;
}

export function calendarDate(value) {
  if (typeof value !== 'string' || !/^\d{4}-\d{2}-\d{2}$/.test(value) || value < '1970-01-01' || value > '9998-12-31') throw new Error('Invalid calendar date');
  const date = new Date(`${value}T00:00:00Z`);
  if (!Number.isFinite(+date) || date.toISOString().slice(0, 10) !== value) throw new Error('Invalid calendar date');
  return value;
}

function instant(value) {
  if (typeof value !== 'string' || !/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d{1,3})?(?:Z|[+-]\d{2}:\d{2})$/.test(value)) throw new Error('Calendar times need an explicit UTC offset');
  calendarDate(value.slice(0, 10));
  const numbers = value.slice(11, 19).split(':').map(Number);
  const offset = /[+-](\d{2}):(\d{2})$/.exec(value);
  if (numbers[0] > 23 || numbers[1] > 59 || numbers[2] > 59 || offset && (+offset[1] > 23 || +offset[2] > 59) || !Number.isFinite(Date.parse(value))) throw new Error('Invalid calendar time');
  return Date.parse(value);
}

export function dateInZone(ms, zone) {
  const parts = new Intl.DateTimeFormat('en-US', { timeZone: zone, year: 'numeric', month: '2-digit', day: '2-digit' }).formatToParts(ms);
  const get = kind => parts.find(p => p.type === kind).value;
  return `${get('year')}-${get('month')}-${get('day')}`;
}

export function shiftDate(date, days) {
  return new Date(Date.parse(`${date}T00:00:00Z`) + days * DAY).toISOString().slice(0, 10);
}

export function calendarRange(date, mode) {
  if (mode === 'day') return [date];
  const current = new Date(`${date}T00:00:00Z`);
  if (mode === 'week') {
    const start = shiftDate(date, -(current.getUTCDay() + 6) % 7);
    return Array.from({ length: 7 }, (_, i) => shiftDate(start, i));
  }
  const first = `${date.slice(0, 7)}-01`;
  const start = shiftDate(first, -(new Date(`${first}T00:00:00Z`).getUTCDay() + 6) % 7);
  return Array.from({ length: 42 }, (_, i) => shiftDate(start, i));
}

export function calendarOnDay(event, date, zone) {
  if (event.end_known === false) return event.all_day ? event.start === date : dateInZone(event.start_ms, zone) === date;
  if (event.all_day) return event.start <= date && event.end > date;
  if (event.calendar_zone === zone) return event.first_day <= date && event.last_day >= date;
  return dateInZone(event.start_ms, zone) <= date && dateInZone(event.end_ms - 1, zone) >= date;
}

export function readCalendar(source) {
  if (typeof source !== 'string' || new TextEncoder().encode(source).length > 1048576) throw new Error('Calendar exceeds the 1 MiB limit');
  const value = JSON.parse(source);
  if (value?.type !== 'ark.calendar' || value.version !== 1 || !Array.isArray(value.events) || value.events.length > 1000) throw new Error('Expected an ark.calendar version 1 file with at most 1,000 items');
  const zone = text(value.timezone, 'timezone', 100);
  try { new Intl.DateTimeFormat('en', { timeZone: zone }).format(); } catch { throw new Error('Calendar needs a valid IANA timezone'); }
  const generated = instant(value.generated_at);
  const ids = new Set();
  const events = value.events.map(item => {
    const id = text(item?.id, 'item id', 128);
    if (ids.has(id)) throw new Error('Calendar item ids must be unique');
    ids.add(id);
    if (!STATES.has(item.state) || !DELIVERY.has(item.delivery) || typeof item.all_day !== 'boolean') throw new Error('Invalid calendar item state or delivery');
    if (item.end_known !== undefined && typeof item.end_known !== 'boolean') throw new Error('Invalid calendar end certainty');
    const start = item.all_day ? calendarDate(item.start) : text(item.start, 'start', 40);
    const end = item.all_day ? calendarDate(item.end) : text(item.end, 'end', 40);
    const startMs = item.all_day ? Date.parse(`${start}T00:00:00Z`) : instant(start);
    const endMs = item.all_day ? Date.parse(`${end}T00:00:00Z`) : instant(end);
    if (endMs <= startMs || endMs - startMs > 366 * DAY) throw new Error('Calendar items need an end after start, within 366 days');
    const event = { id, title: text(item.title, 'title'), state: item.state, delivery: item.delivery, all_day: item.all_day, end_known: item.end_known !== false, start, end, start_ms: startMs, end_ms: endMs };
    if (!item.all_day) Object.assign(event, { calendar_zone: zone, first_day: dateInZone(startMs, zone), last_day: dateInZone(endMs - 1, zone) });
    for (const key of ['account', 'next_action', 'draft', 'receipt', 'source_record', 'source_revision']) {
      if (item[key] !== undefined) event[key] = text(item[key], key, key === 'next_action' ? 2000 : 512);
    }
    return event;
  }).sort((a, b) => a.start_ms - b.start_ms || a.id.localeCompare(b.id));
  return { title: text(value.title, 'title'), timezone: zone, generated_ms: generated, events };
}
