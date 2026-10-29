import test from 'node:test';
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
const code=await readFile(new URL('../../web/js/calendar-data.js',import.meta.url),'utf8');
const {readCalendar,calendarOnDay,calendarRange}=await import(`data:text/javascript;base64,${Buffer.from(code).toString('base64')}`);
const event={id:'draft',title:'Draft',start:'2026-11-01T01:30:00-04:00',end:'2026-11-01T01:45:00-04:00',all_day:false,state:'draft',delivery:'manual_reminder'};
const source=events=>JSON.stringify({type:'ark.calendar',version:1,title:'Work calendar',timezone:'America/New_York',generated_at:'2026-10-29T12:00:00Z',events});
test('calendar preserves distinct DST fold instants, gap instants and exclusive all-day dates',()=>{
 const data=readCalendar(source([event,{...event,id:'second',start:'2026-11-01T01:30:00-05:00',end:'2026-11-01T01:45:00-05:00'},{...event,id:'gap',start:'2026-03-08T03:30:00-04:00',end:'2026-03-08T04:00:00-04:00'},{...event,id:'all',all_day:true,start:'2026-11-01',end:'2026-11-03'}]));
 const first=data.events.find(e=>e.id==='draft'),second=data.events.find(e=>e.id==='second');assert.equal(second.start_ms-first.start_ms,3600000);
 assert.equal(calendarOnDay(first,'2026-11-01',data.timezone),true);assert.equal(calendarOnDay(first,'2026-10-31',data.timezone),false);
 const all=data.events.find(e=>e.id==='all');assert.equal(calendarOnDay(all,'2026-11-02',data.timezone),true);assert.equal(calendarOnDay(all,'2026-11-03',data.timezone),false);
 const midnight=readCalendar(source([{...event,start:'2026-11-01T23:00:00-05:00',end:'2026-11-02T00:00:00-05:00'}])).events[0];assert.equal(calendarOnDay(midnight,'2026-11-02',data.timezone),false);
 assert.equal(calendarRange('2026-03-08','week')[0],'2026-03-02');assert.equal(calendarRange('2026-11-01','month')[0],'2026-10-26');
});
test('calendar refuses ambiguous times, invalid dates, duplicate ids and excessive input',()=>{
 for(const change of [{start:'2026-11-01T01:30:00'},{start:'2026-02-30T12:00:00Z'},{end:event.start},{all_day:true,start:'2026-02-30',end:'2026-03-02'},{state:'verified_automatically'},{delivery:'auto_publish'},{title:'x'.repeat(241)}])assert.throws(()=>readCalendar(source([{...event,...change}])));
 assert.throws(()=>readCalendar(source([event,event])),/unique/);assert.throws(()=>readCalendar(source(Array.from({length:1001},(_,i)=>({...event,id:String(i)})))),/1,000/);
 assert.throws(()=>readCalendar(source([]).replace('America/New_York','Not/AZone')),/IANA/);assert.throws(()=>readCalendar(' '.repeat(1048577)),/1 MiB/);
 assert.throws(()=>readCalendar(source([{...event,start:'2026-11-01T24:00:00Z'}])));assert.equal(readCalendar(source([{...event,title:'<img src=x onerror=alert(1)>'}])).events[0].title,'<img src=x onerror=alert(1)>');
});

test('unknown end markers preserve certainty and refuse nonboolean claims',()=>{
 assert.equal(readCalendar(source([{...event,end_known:false}])).events[0].end_known,false);
 assert.equal(readCalendar(source([event])).events[0].end_known,true);
 const marker=readCalendar(source([{...event,end_known:false,start:'2026-11-01T23:59:50-05:00',end:'2026-11-02T00:00:50-05:00'}])).events[0];
 assert.equal(calendarOnDay(marker,'2026-11-01','America/New_York'),true);
 assert.equal(calendarOnDay(marker,'2026-11-02','America/New_York'),false);
 assert.throws(()=>readCalendar(source([{...event,end_known:'false'}])),/end certainty/);
});
